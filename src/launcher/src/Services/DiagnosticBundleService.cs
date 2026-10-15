using System.IO.Compression;
using System.Net;
using System.Net.NetworkInformation;
using System.Net.Sockets;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace SplinterCellCNLauncher.Services;

public sealed class DiagnosticState
{
    public bool HostMode { get; init; }
    public string GameplayMode { get; init; } = "Official";
    public double Fov { get; init; } = 1.2;
    public string ServerAddress { get; init; } = "";
    public string BindAddress { get; init; } = "";
    public string HostBindAddress { get; init; } = "";
    public int ServerStatus { get; init; }
    public bool ServerReachable { get; init; }
    public bool GameStarting { get; init; }
    public bool GameRunning { get; init; }
    public bool HostRunning { get; init; }
}

// Only bounded, categorized in-memory events; never read or include raw log files.
public static class DiagnosticEvents
{
    public sealed record Entry(DateTimeOffset Utc,string Level,string Code,string? ExceptionType,int? HResult,int? SocketError);
    private static readonly object Gate=new();
    private static readonly Queue<Entry> Entries=new();
    private static void Add(Entry entry)
    {
        lock(Gate){Entries.Enqueue(entry);while(Entries.Count>200)Entries.Dequeue();}
        RunDiagnosticStore.Record(entry);
    }
    public static void Record(string level,string message)
    {
        string code = message.Contains("authentication",StringComparison.OrdinalIgnoreCase)?"authentication":
            message.Contains("configuration",StringComparison.OrdinalIgnoreCase)?"configuration":
            message.Contains("discovery",StringComparison.OrdinalIgnoreCase)?"discovery":
            message.Contains("broadcast",StringComparison.OrdinalIgnoreCase)?"broadcast":
            message.Contains("game",StringComparison.OrdinalIgnoreCase)?"game-lifecycle":
            message.Contains("server",StringComparison.OrdinalIgnoreCase)?"server":
            message.Contains("password",StringComparison.OrdinalIgnoreCase)?"credential-store":"launcher";
        Add(new(DateTimeOffset.UtcNow,level=="ERROR"?"ERROR":"INFO",code,null,null,null));
    }
    public static void RecordException(Exception error)
    {
        Add(new(DateTimeOffset.UtcNow,"ERROR","exception",error.GetType().Name,error.HResult,
            error is SocketException socket?(int)socket.SocketErrorCode:null));
    }
    public static Entry[] Snapshot(){lock(Gate)return Entries.ToArray();}
}

public static class DiagnosticBundleService
{
    private static readonly string[] EntryNames=["README.txt","diagnostics.json","runtime.json","session-events.json","runs.json","sources.json","projected-events.json","config-summary.json"];
    private static readonly int[] FixedPorts=[80,8000,50051,21126,21127,47777];
    private static readonly JsonSerializerOptions JsonOptions=new(){WriteIndented=true};
    // Existing call signature retained. Full export never calls the legacy store's cleanup/flush.
    public static string Export(string destination,DiagnosticState state,string? launcherRoot=null,bool overwrite=false,bool collectNetwork=true)
        =>ExportRetained(destination,state,launcherRoot,overwrite,collectNetwork);
    public static string ExportRetained(string destination,DiagnosticState state,string? launcherRoot=null,bool overwrite=false,bool collectNetwork=true,
        string? gameRoot=null,DiagnosticExportBudget? budget=null,CancellationToken cancellationToken=default)
    {
        using var context=new DiagnosticExportContext(budget??new(),cancellationToken);
        destination=Path.GetFullPath(destination);
        ValidatePath(destination);
        if(!Path.GetExtension(destination).Equals(".zip",StringComparison.OrdinalIgnoreCase))throw new ArgumentException("ZIP destination required.");
        string root=Path.GetFullPath(launcherRoot??AppContext.BaseDirectory);ValidatePath(root);
        if(gameRoot!=null)ValidatePath(Path.GetFullPath(gameRoot));
        string dataRoot=launcherRoot==null?LogService.AppDataDir:Path.Combine(root,"data",ScblLan.LocalData.Segment(Environment.MachineName),ScblLan.LocalData.Segment(Environment.UserName));
        var plan=new DiagnosticExportPlan(dataRoot,gameRoot,context);
        if(plan.Sources.Any(s=>s.Path.Length>0&&s.Path.Equals(destination,StringComparison.OrdinalIgnoreCase)))throw new IOException("Destination is a diagnostic source.");
        string temp=destination+"."+Guid.NewGuid().ToString("N")+".tmp";
        var expected=new Dictionary<string,Payload>(StringComparer.OrdinalIgnoreCase);
        try
        {
            using(var file=new FileStream(temp,FileMode.CreateNew,FileAccess.Write,FileShare.None,16384,FileOptions.Asynchronous))
            using(var bounded=new ZipWriteStream(file,context))
            using(var zip=new ZipArchive(bounded,ZipArchiveMode.Create))
            {
                void Header(string name,object value)=>Text(name,JsonSerializer.SerializeToUtf8Bytes(value,JsonOptions));
                void Text(string name,byte[] bytes)
                {context.Output(bytes.Length);using var stream=Open(name);stream.WriteAsync(bytes.AsMemory(),context.Token).AsTask().GetAwaiter().GetResult();expected[name]=new(bytes.LongLength,Convert.ToHexString(SHA256.HashData(bytes)));}
                Stream Open(string name)
                {
                    context.Check();if(!SafeEntry(name)||expected.ContainsKey(name))throw new InvalidDataException("Diagnostic entry plan mismatch.");
                    expected.Add(name,new(0,""));return zip.CreateEntry(name,CompressionLevel.Optimal).Open();
                }
                foreach(var source in plan.Sources.Where(s=>s.Entry!=null))
                {
                    context.Check();string? entry=source.Entry;
                    FileStream? input=null;
                    try
                    {
                        if(!DiagnosticSafety.Plain(source.Path)){source.Status="ReparsePointSkipped";source.Entry=null;continue;}
                        if(!File.Exists(source.Path)){source.Status=source.Kind=="EffectiveConfigSnapshot"?"MissingHistoricalSnapshot":"Missing";source.Entry=null;continue;}
                        input=new FileStream(source.Path,FileMode.Open,FileAccess.Read,FileShare.ReadWrite|FileShare.Delete,16384,FileOptions.Asynchronous);
                        if(!DiagnosticSafety.Plain(source.Path))throw new IOException("Diagnostic boundary changed.");
                        long length=input.Length;source.SourceBytesAtOpen=length;source.SnapshotStart=0;source.SnapshotEnd=length;
                        if(length>context.Budget.MaxFileInputBytes)throw new InvalidDataException("Diagnostic single input limit exceeded; no archive published.");
                        var identity=Identity(input);DateTime written=File.GetLastWriteTimeUtc(source.Path);
                        byte[]? structured=null;
                        if(source.Kind!="Text")
                        {
                            try{structured=DiagnosticStructuredExport.Read(input,source,context);}
                            catch(RequestedConfigurationSchemaException e){source.Status=e.Code;source.ErrorType="UnsupportedSnapshot";source.Entry=null;continue;}
                            catch(Exception e)when(e is JsonException or InvalidOperationException or FormatException or NullReferenceException or KeyNotFoundException)
                            {source.Status="ParseFailed";source.ErrorType="UnsupportedSchema";source.Entry=null;continue;}
                        }
                        using(var output=Open(entry!))
                        using(var hash=new HashWriteStream(output))
                        {
                            if(structured==null)DiagnosticTextSanitizer.Copy(input,hash,length,source,context);
                            else{context.Output(structured.Length);hash.WriteAsync(structured.AsMemory(),context.Token).AsTask().GetAwaiter().GetResult();source.ExportBytes=structured.Length;if(source.Kind=="Run")context.RunSummary(structured);}
                            source.ExportSha256=hash.Hash();expected[entry!]=new(source.ExportBytes,source.ExportSha256);
                        }
                        source.ReadComplete=source.ReadBytes==length;
                        source.Consistency="BestEffortSnapshot";
                        try
                        {
                            if(!DiagnosticSafety.Plain(source.Path)||input.Length!=length||File.GetLastWriteTimeUtc(source.Path)!=written)source.Consistency="Changed";
                            else{using var current=new FileStream(source.Path,FileMode.Open,FileAccess.Read,FileShare.ReadWrite|FileShare.Delete);if(Identity(current)!=identity)source.Consistency="Changed";}
                        }
                        catch(IOException){source.Consistency="Unknown";}
                        catch(UnauthorizedAccessException){source.Consistency="Unknown";}
                        source.Status=source.Consistency=="BestEffortSnapshot"?"Read":"SourceChanged";
                        if(source.Kind=="Run" && source.Status=="Read" && structured!=null) {
                            var metadata=JsonSerializer.Deserialize<RunSummary>(structured,DiagnosticSafety.Json)!;
                            context.SnapshotRuns[metadata.RunId]=source.SnapshotMetadata;
                        }
                    }
                    catch(Exception e)when((e is IOException && e is not InvalidDataException)||e is UnauthorizedAccessException)
                    {
                        // No partial payload is published on a mid-read failure.
                        if(entry!=null&&expected.ContainsKey(entry))throw;
                        source.Status="Unavailable";source.ErrorType=e is UnauthorizedAccessException?"AccessDenied":"IOError";source.Entry=null;
                    }
                    finally{input?.Dispose();}
                }
                string[] allowed=["SCBL.LAN.exe","runtime/uplay_r1_loader.dll","runtime/scbl_gameplay.dll","runtime/SCBL.LAN.Server.exe"];
                var runtime=allowed.Select(relative=>InspectRuntimeBounded(root,relative,context)).ToArray();
                var diagnostics=new {Schema=3,Product="SCBL LAN",Version=typeof(DiagnosticBundleService).Assembly.GetName().Version?.ToString(),CreatedUtc=DateTimeOffset.UtcNow,
                    Graphics="DX11",CurrentUiStateOnly=true,Mode=GameplayRuntime.Normalize(state.GameplayMode),Fov=GameplayRuntime.NormalizeFov(state.Fov),state.HostMode,
                    Server=DiagnosticStructuredExport.Endpoint(state.ServerAddress),BindIPv4=DiagnosticStructuredExport.IPv4(state.BindAddress)??"",
                    ServerStatus=Math.Clamp(state.ServerStatus,0,32),state.ServerReachable,state.GameStarting,state.GameRunning,state.HostRunning,FixedPorts,
                    Network=collectNetwork?NetworkSnapshot():new{Status="NotCollectedInIsolatedFixture"} as object,
                    Scope="RetainedProducerLogsAndPersistedConfigs",PackageComplete=false,SummaryOnly=true,
                    Privacy="Known-rule minimized full retained text may still contain sensitive data. Review before sharing. No upload; databases, credentials, dumps and saves excluded. Persisted configuration schemas are exported after typed redaction; new launch snapshots record requested values only; native loaded values are not observed; older runs may have no snapshot."};
                Text("README.txt",Encoding.UTF8.GetBytes("SCBL LAN retained-log diagnostics / 现存日志诊断包\nScope=RetainedProducerLogsAndPersistedConfigs; PackageComplete=false; ScopeIncomplete=true. 已持久化配置按schema脱敏导出；新启动记录本次请求配置；不证明native加载值，旧run缺失不补填。\nFull retained text is minimized with known rules, not a universal privacy guarantee. 日志全文可能仍含敏感信息，分享前请检查；不会自动上传。\nExisting source files were only read; summaries are not a complete history. 当前快照不是全局一致性或已加载证明；已删除/覆盖/裁剪历史无法恢复。\nSee manifest.json for every source, byte range, omission, consistency, hash and scope gap. DB/credentials/dumps/exception details/saves are excluded.\n"));
                Header("diagnostics.json",diagnostics);Header("runtime.json",runtime);Header("session-events.json",DiagnosticStructuredExport.Session());
                Text("runs.json",JsonSerializer.SerializeToUtf8Bytes(new{Schema=2,SummaryOnly=true,Status="Read",Window="Recent4ByStartedUtcThenRunId; full retained roles in manifest",PersistedSnapshotsNotLiveProcessClaims=true,Runs=context.RunSummaries.ToArray()},DiagnosticSafety.Json));
                Header("sources.json",plan.Sources.Select(s=>new{s.Id,s.Status,Bytes=s.SourceBytesAtOpen,LinesRead=s.Lines,s.Events,s.EventsRecognized,s.Dropped,s.UnknownFields,OmittedPrefixBytes=0,SummaryOnly=true}));
                Header("projected-events.json",context.Projected.Select(e=>e.Event).OrderBy(e=>e.Utc).ToArray());Header("config-summary.json",DiagnosticStructuredExport.ConfigSummary(dataRoot,state.HostBindAddress,context));
                bool payloadComplete=plan.Sources.Where(s=>s.Kind!="ScopeGap").All(s=>s.Status=="Read"&&s.ReadComplete&&s.Consistency=="BestEffortSnapshot");
                Header("manifest.json",new{Schema=1,Scope="RetainedProducerLogsAndPersistedConfigs",PackageComplete=false,ScopeIncomplete=true,plan.DiscoveryComplete,PayloadComplete=payloadComplete,
                    HistoryCompleteness="UnknownOrAlreadyDiscarded",Consistency="BestEffortPerFileSnapshotNotGlobalSnapshot",
                    ConfigurationStatus="PersistedSchemasImplemented",EffectiveConfigurationStatus="RequestedSnapshotWriterImplementedNativeLoadedNotObserved",ThirdPartyLogsStatus="Unsupported",UnknownUnenumeratedRanges=plan.DiscoveryComplete?"None":"Unknown",
                    Budget=context.Budget,context.DiscoveryEntries,context.InputBytes,Sources=plan.Sources,SummaryWindow="Last256PerSourceThenLast1024DiscoveredSortedUtc; no full-text truncation",context.GlobalSummaryEventsDropped,
                    SummaryRoles=EntryNames.Select(n=>new{Name=n,SummaryOnly=true}),Payloads=expected.Select(p=>new{Name=p.Key,p.Value.Bytes,p.Value.Sha256}).ToArray()});
                context.Check();if(file.Position>context.Budget.MaxZipBytes)throw new InvalidDataException("Diagnostic ZIP limit exceeded.");
            }
            context.Check();if(new FileInfo(temp).Length>context.Budget.MaxZipBytes)throw new InvalidDataException("Diagnostic ZIP limit exceeded.");
            using(var zip=ZipFile.OpenRead(temp))
            {
                if(zip.Entries.Count!=expected.Count||!zip.Entries.Select(e=>e.FullName).ToHashSet(StringComparer.OrdinalIgnoreCase).SetEquals(expected.Keys))throw new InvalidDataException("Diagnostic exact entry mismatch.");
                foreach(var entry in zip.Entries)
                {
                    context.Check();var payload=expected[entry.FullName];if(entry.Length!=payload.Bytes)throw new InvalidDataException("Diagnostic payload size mismatch.");
                    using var input=entry.Open();using var hash=IncrementalHash.CreateHash(HashAlgorithmName.SHA256);byte[] buffer=new byte[16384];long size=0;
                    while(true){context.Check();int n=input.ReadAsync(buffer.AsMemory(),context.Token).AsTask().GetAwaiter().GetResult();if(n==0)break;size+=n;if(size>payload.Bytes)throw new InvalidDataException("Diagnostic payload length mismatch.");hash.AppendData(buffer,0,n);}
                    if(size!=payload.Bytes||Convert.ToHexString(hash.GetHashAndReset())!=payload.Sha256)throw new InvalidDataException("Diagnostic payload hash mismatch.");
                    if(!entry.FullName.StartsWith("logs/",StringComparison.Ordinal) && entry.FullName.EndsWith(".json",StringComparison.Ordinal)){using var stream=entry.Open();using var document=JsonDocument.Parse(stream,new(){MaxDepth=context.Budget.MaxJsonDepth});}
                }
            }
            context.Check();ValidatePath(destination);ValidatePath(temp);File.Move(temp,destination,overwrite);return destination;
        }
        finally{if(File.Exists(temp))File.Delete(temp);}
    }
    private sealed class ZipWriteStream(FileStream file,DiagnosticExportContext context):Stream
    {
        private void Guard(int count){context.Check();if(file.Position+count>context.Budget.MaxZipBytes)throw new InvalidDataException("Diagnostic ZIP byte limit exceeded; no archive published.");}
        public override void Write(byte[] b,int offset,int count){Guard(count);file.WriteAsync(b.AsMemory(offset,count),context.Token).AsTask().GetAwaiter().GetResult();}
        public override void Write(ReadOnlySpan<byte> b){Guard(b.Length);file.Write(b);}
        public override async ValueTask WriteAsync(ReadOnlyMemory<byte> b,CancellationToken token=default){Guard(b.Length);await file.WriteAsync(b,context.Token).ConfigureAwait(false);}
        public override bool CanRead=>false;public override bool CanSeek=>true;public override bool CanWrite=>true;public override long Length=>file.Length;public override long Position{get=>file.Position;set{context.Check();file.Position=value;}}
        public override void Flush(){context.Check();file.Flush();}public override int Read(byte[] b,int o,int n)=>throw new NotSupportedException();public override long Seek(long o,SeekOrigin s){context.Check();return file.Seek(o,s);}public override void SetLength(long n){context.Check();if(n>context.Budget.MaxZipBytes)throw new InvalidDataException("Diagnostic ZIP byte limit exceeded.");file.SetLength(n);}
    }
    private sealed record Payload(long Bytes,string Sha256);
    private static bool SafeEntry(string name)=>name.Length<240 && !name.StartsWith('/')&&!name.Contains('\\')&&!name.Contains(':')&&name.Split('/').All(p=>p.Length>0&&p!="."&&p!="..");
    private static void ValidatePath(string path)
    {
        if(path.StartsWith("\\\\",StringComparison.Ordinal)||path.Skip(2).Contains(':')||!DiagnosticSafety.Plain(path))throw new IOException("Unsupported or reparse diagnostic path.");
    }
    private sealed class HashWriteStream(Stream inner):Stream
    {
        private readonly IncrementalHash hash=IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        internal string Hash()=>Convert.ToHexString(hash.GetHashAndReset());
        public override void Write(byte[] b,int offset,int count){hash.AppendData(b,offset,count);inner.Write(b,offset,count);}
        public override async ValueTask WriteAsync(ReadOnlyMemory<byte> b,CancellationToken token=default){hash.AppendData(b.Span);await inner.WriteAsync(b,token).ConfigureAwait(false);}
        public override bool CanRead=>false;public override bool CanSeek=>false;public override bool CanWrite=>true;public override long Length=>throw new NotSupportedException();public override long Position{get=>throw new NotSupportedException();set=>throw new NotSupportedException();}
        public override void Flush()=>inner.Flush();public override int Read(byte[] b,int o,int n)=>throw new NotSupportedException();public override long Seek(long o,SeekOrigin s)=>throw new NotSupportedException();public override void SetLength(long n)=>throw new NotSupportedException();
        protected override void Dispose(bool disposing){if(disposing)hash.Dispose();base.Dispose(disposing);}
    }
    [StructLayout(LayoutKind.Sequential)]private struct FileInformation{public uint Attributes;public System.Runtime.InteropServices.ComTypes.FILETIME Creation,Access,Write;public uint Volume,SizeHigh,SizeLow,Links,IndexHigh,IndexLow;}
    [DllImport("kernel32.dll",SetLastError=true)]private static extern bool GetFileInformationByHandle(Microsoft.Win32.SafeHandles.SafeFileHandle handle,out FileInformation information);
    private static (uint Volume,uint High,uint Low) Identity(FileStream file)
    {if(!GetFileInformationByHandle(file.SafeFileHandle,out var info))throw new IOException("Diagnostic identity unavailable.");return(info.Volume,info.IndexHigh,info.IndexLow);}
    private static Component InspectRuntimeBounded(string root,string relative,DiagnosticExportContext context)
    {
        context.Check();string path=Path.Combine(root,relative);
        try
        {
            if(!DiagnosticSafety.Plain(path))return new(relative,"ReparsePointSkipped",null,null,null);
            if(!File.Exists(path))return new(relative,"Missing",null,null,null);
            using var stream=new FileStream(path,FileMode.Open,FileAccess.Read,FileShare.ReadWrite|FileShare.Delete,16384,FileOptions.Asynchronous);
            long length=stream.Length;if(length>context.Budget.MaxFileInputBytes)return new(relative,"TooLarge",length,null,null);
            using var hash=IncrementalHash.CreateHash(HashAlgorithmName.SHA256);byte[] bytes=new byte[16384];long left=length;
            while(left>0){context.Check();int n=stream.ReadAsync(bytes.AsMemory(0,(int)Math.Min(left,bytes.Length)),context.Token).AsTask().GetAwaiter().GetResult();if(n==0)throw new IOException();context.Input(n);hash.AppendData(bytes,0,n);left-=n;}
            return new(relative,"Read",length,Convert.ToHexString(hash.GetHashAndReset()),null);
        }
        catch(Exception e)when((e is IOException&&e is not InvalidDataException)||e is UnauthorizedAccessException){return new(relative,"Unavailable",null,null,"IOError");}
    }
    private sealed record Component(string Name,string Status,long? Bytes,string? Sha256,string? Error);
    private static object NetworkSnapshot()
    {
        try {
            var properties=IPGlobalProperties.GetIPGlobalProperties();
            object[] Endpoints(IEnumerable<IPEndPoint> endpoints)=>endpoints.Where(e=>FixedPorts.Contains(e.Port) && e.AddressFamily==AddressFamily.InterNetwork)
                .OrderBy(e=>e.Port).Take(64).Select(e=>(object)new{IPv4=e.Address.ToString(),e.Port}).ToArray();
            return new {Status="ReadOnlySnapshot",TcpListeners=Endpoints(properties.GetActiveTcpListeners()),UdpListeners=Endpoints(properties.GetActiveUdpListeners()),
                IPv4=NetworkInterface.GetAllNetworkInterfaces().Where(n=>n.OperationalStatus==OperationalStatus.Up)
                    .SelectMany(n=>n.GetIPProperties().UnicastAddresses).Where(a=>a.Address.AddressFamily==AddressFamily.InterNetwork)
                    .Select(a=>a.Address.ToString()).Distinct().Take(32).ToArray()};
        } catch(Exception e) when(e is NetworkInformationException or SocketException or System.Security.SecurityException) {
            return new {Status="Unavailable",Error=e.GetType().Name};
        }
    }
}
