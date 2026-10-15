using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Text.Json;

namespace SplinterCellCNLauncher.Services;

public sealed record DiagnosticExportBudget
{
    public int MaxFiles { get; init; } = 2048;
    public int MaxDiscoveryEntries { get; init; } = 8192;
    public long MaxFileInputBytes { get; init; } = 32L*1024*1024;
    public long MaxTotalInputBytes { get; init; } = 128L*1024*1024;
    public long MaxOutputBytes { get; init; } = 256L*1024*1024;
    public long MaxZipBytes { get; init; } = 128L*1024*1024;
    public int MaxLineBytes { get; init; } = 65536;
    public int MaxStructuredBytes { get; init; } = 512*1024;
    public int MaxJsonDepth { get; init; } = 16;
    public int MaxJsonNodes { get; init; } = 32768;
    public int DeadlineMilliseconds { get; init; } = 30000;
    internal void Validate()
    {
        if (MaxFiles is <1 or >2048 || MaxDiscoveryEntries is <1 or >8192 || MaxFileInputBytes is <1 or >33554432 ||
            MaxTotalInputBytes is <1 or >134217728 || MaxOutputBytes is <1 or >268435456 || MaxZipBytes is <1 or >134217728 ||
            MaxLineBytes is <1 or >65536 || MaxStructuredBytes is <1 or >524288 || MaxJsonDepth is <1 or >16 ||
            MaxJsonNodes is <1 or >32768 || DeadlineMilliseconds is <1 or >30000) throw new ArgumentOutOfRangeException(nameof(DiagnosticExportBudget));
    }
}

internal sealed class DiagnosticExportContext : IDisposable
{
    internal readonly DiagnosticExportBudget Budget;
    private readonly CancellationTokenSource stop;
    private readonly Stopwatch clock=Stopwatch.StartNew();
    internal CancellationToken Token=>stop.Token;
    internal long InputBytes,OutputBytes;
    internal int DiscoveryEntries;
    internal readonly Queue<(DiagnosticSourceProjection.Event Event,DiagnosticExportSource Source)> Projected=[];
    internal readonly List<RunSummary> RunSummaries=[];
    internal readonly Dictionary<Guid,RequestedConfigurationStatus?> SnapshotRuns=[];
    internal int GlobalSummaryEventsDropped;
    internal void Projection(DiagnosticExportSource source,IEnumerable<DiagnosticSourceProjection.Event> events)
    {
        foreach(var e in events){Check();Projected.Enqueue((e,source));source.Events++;if(Projected.Count>1024){var removed=Projected.Dequeue();removed.Source.Events--;removed.Source.Dropped++;GlobalSummaryEventsDropped++;}}
    }
    internal void RunSummary(byte[] sanitized)
    {
        Check();var s=JsonSerializer.Deserialize<RunSummary>(sanitized,DiagnosticSafety.Json);var clean=s==null?null:DiagnosticSafety.Clean(s);
        // At most five candidates at a time; directory enumeration order is not recency.
        if(clean!=null){RunSummaries.Add(clean);RunSummaries.Sort((a,b)=>{int utc=b.StartedUtc.CompareTo(a.StartedUtc);return utc!=0?utc:StringComparer.Ordinal.Compare(a.RunId.ToString("N"),b.RunId.ToString("N"));});if(RunSummaries.Count>4)RunSummaries.RemoveAt(4);}
    }
    internal DiagnosticExportContext(DiagnosticExportBudget budget,CancellationToken token)
    {budget.Validate();Budget=budget;stop=CancellationTokenSource.CreateLinkedTokenSource(token);stop.CancelAfter(budget.DeadlineMilliseconds);}
    internal void Check(){Token.ThrowIfCancellationRequested();if(clock.ElapsedMilliseconds>=Budget.DeadlineMilliseconds)throw new OperationCanceledException("Diagnostic deadline exceeded.",Token);}
    internal void Discover(){Check();if(++DiscoveryEntries>Budget.MaxDiscoveryEntries)throw new InvalidDataException("Diagnostic discovery limit exceeded; no archive published.");}
    internal void Input(int bytes){Check();InputBytes=checked(InputBytes+bytes);if(InputBytes>Budget.MaxTotalInputBytes)throw new InvalidDataException("Diagnostic input limit exceeded; no archive published.");}
    internal void Output(int bytes){Check();OutputBytes=checked(OutputBytes+bytes);if(OutputBytes>Budget.MaxOutputBytes)throw new InvalidDataException("Diagnostic output limit exceeded; no archive published.");}
    public void Dispose()=>stop.Dispose();
}

internal sealed class DiagnosticExportSource(string id,string path,string? entry,string kind,string retention)
{
    internal string Path {get;}=path;
    internal RequestedConfigurationStatus? SnapshotMetadata; // Internal source binding; never serialized into manifest.
    public string Id {get;}=id;
    public string? Entry {get;set;}=entry;
    public string Kind {get;}=kind;
    public string Status {get;set;}="Pending";
    public long? SourceBytesAtOpen {get;set;}
    public long ReadBytes {get;set;}
    public long ExportBytes {get;set;}
    public string? ExportSha256 {get;set;}
    public int Lines {get;set;}
    public int Redactions {get;set;}
    public int InvalidValues {get;set;}
    public int UnknownFields {get;set;}
    public int Events {get;set;}
    public int EventsRecognized {get;set;}
    public int Dropped {get;set;}
    public string Consistency {get;set;}="NotRead";
    public long? SnapshotStart {get;set;}
    public long? SnapshotEnd {get;set;}
    public bool ReadComplete {get;set;}
    public string ProducerRetention {get;}=retention;
    public string HistoryCompleteness {get;}="UnknownOrAlreadyDiscarded";
    public string? ErrorType {get;set;}
}

internal sealed class DiagnosticExportPlan
{
    internal readonly List<DiagnosticExportSource> Sources=[];
    internal bool DiscoveryComplete=true;
    internal DiagnosticExportPlan(string dataRoot,string? gameRoot,DiagnosticExportContext context)
    {
        string root=System.IO.Path.GetFullPath(dataRoot);
        Add("launcher",root,"launcher.log","logs/launcher/launcher.log","Text","AppendNoRotation");
        Add("legacy-launcher",root,"logs/launcher.log","logs/legacy/launcher.log","Text","LegacyOptionalAppend");
        Add("fallback-main",root,"logs/game/bl-tracing.log","logs/fallback/bl-tracing.log","Text","ReplacedOnAttach");
        Add("fallback-party",root,"logs/game/hooks-party-trace.log","logs/fallback/hooks-party-trace.log","Text","AppendNoRotation");
        Add("launcher-config",root,"launcher_settings.json","configs/launcher_settings.redacted.json","ConfigLauncher","PersistedCurrentSchema");Add("legacy-config",root,"settings.json","configs/settings.redacted.json","ConfigLegacy","LegacyOptional");
        foreach(string run in Dirs(System.IO.Path.Combine(root,"diagnostics-v2"),"runs"))
        {
            string name=System.IO.Path.GetFileName(run);
            if(!Guid.TryParseExact(name,"N",out var id)||name!=id.ToString("N"))continue;
            string prefix="run:"+name;
            Add(prefix+":main",run,"native/logs/game/bl-tracing.log","logs/runs/"+name+"/bl-tracing.log","Text","ReplacedOnAttachClosedOver8MiBTail256KiB");
            Add(prefix+":party",run,"native/logs/game/hooks-party-trace.log","logs/runs/"+name+"/hooks-party-trace.log","Text","AppendClosedOver8MiBTail256KiB");
            Add(prefix+":metadata",run,"run.json","runs/"+name+"/run.json","Run","CountBoundedExactFileRetention");
            Add(prefix+":effective-config",run,"effective-config.redacted.json","runs/"+name+"/effective-config.redacted.json","EffectiveConfigSnapshot","ImmutableRequestedLaunchConfiguration");
            foreach(string capture in Dirs(System.IO.Path.Combine(run,"capture"),prefix+":capture"))
            {
                string child=System.IO.Path.GetFileName(capture);string[] pieces=child.Split('-');
                if(pieces.Length!=2||!int.TryParse(pieces[0],out int pid)||pid<=0||!long.TryParse(pieces[1],out long created)||created<=0||child!=$"{pid}-{created}")continue;
                Add(prefix+":capture:"+child,capture,"state.json","runs/"+name+"/capture/"+child+"/state.json","CaptureState","PrivateStateOnlyNoDumpOrExceptionDetails");
            }
        }
        foreach(string host in Dirs(System.IO.Path.Combine(root,"server"),"hosts"))
        {
            string name=System.IO.Path.GetFileName(host);
            if(!IPAddress.TryParse(name.Replace('_','.'),out var ip)||ip.AddressFamily!=AddressFamily.InterNetwork||name!=ip.ToString().Replace('.','_'))continue;
            foreach(string n in new[]{"server.log.json","server.stdout.txt","server.stderr.txt","init.stdout.txt","init.stderr.txt"})
                Add("host:"+name+":"+n,host,n,"logs/hosts/"+name+"/"+n,"Text",n.StartsWith("init.")?"ReplaceAfterCompletedRead":n=="server.log.json"?"StartupRotation10":"AppendNoRotation");
            for(int i=1;i<=10;i++){string n="server.log.json."+i;Add("host:"+name+":"+n,host,n,"logs/hosts/"+name+"/"+n,"Text","StartupRotation10MayFail");}
            Add("host:"+name+":service-config",host,"service.toml","configs/hosts/"+name+"/service.redacted.json","ConfigService","PersistedCurrentSchema");Add("host:"+name+":balancing-config",host,"data/mp_balancing.ini","configs/hosts/"+name+"/mp_balancing.redacted.json","ConfigBalancing","PersistedCurrentSchema");
        }
        if(gameRoot==null){Gap("game-fallback","GameContextNotSelected");Gap("game-launch-reference","GameContextNotSelected");Gap("game-legacy-config","GameContextNotSelected");}
        else
        {
            string game=System.IO.Path.GetFullPath(gameRoot);
            Add("game-fallback",game,"bl-tracing.log","logs/game-fallback/bl-tracing.log","Text","ReplacedOnAttachFallback");
            Add("game-launch-reference",game,"scbl-lan.toml","configs/game/scbl-lan.redacted.json","ConfigReference","EphemeralSessionReference");Add("game-legacy-config",game,"scbl.toml","configs/game/scbl.redacted.json","ConfigLegacyHooks","LegacyOptionalNotCurrentLoadedConfig");
        }
        void Add(string id,string boundary,string relative,string entry,string kind,string retention)
        {
            context.Check();if(Sources.Count>=context.Budget.MaxFiles)throw new InvalidDataException("Diagnostic source limit exceeded; no archive published.");
            string path=System.IO.Path.GetFullPath(System.IO.Path.Combine(boundary,relative));
            if(!path.StartsWith(System.IO.Path.GetFullPath(boundary).TrimEnd(System.IO.Path.DirectorySeparatorChar)+System.IO.Path.DirectorySeparatorChar,StringComparison.OrdinalIgnoreCase))throw new InvalidDataException("Invalid diagnostic source boundary.");
            Sources.Add(new(id,path,entry,kind,retention));
        }
        void Gap(string id,string status)
        {
            context.Check();if(Sources.Count>=context.Budget.MaxFiles)throw new InvalidDataException("Diagnostic source limit exceeded; no archive published.");
            Sources.Add(new(id,"",null,"ScopeGap","NotCollected"){Status=status});
        }
        IEnumerable<string> Dirs(string path,string id)
        {
            context.Check();
            if(!DiagnosticSafety.Plain(path)){DiscoveryComplete=false;Gap(id,"ReparsePointSkipped");yield break;}
            if(!Directory.Exists(path))yield break;
            IEnumerator<string>? iterator=null;
            try{iterator=Directory.EnumerateDirectories(path).GetEnumerator();}
            catch(Exception e) when(e is IOException or UnauthorizedAccessException){DiscoveryComplete=false;Gap(id,"DirectoryUnavailable");}
            if(iterator==null)yield break;
            using(iterator)while(true)
            {
                context.Check();bool more;
                try{more=iterator.MoveNext();}
                catch(Exception e) when(e is IOException or UnauthorizedAccessException){DiscoveryComplete=false;Gap(id,"DirectoryUnavailable");yield break;}
                if(!more)yield break;
                context.Discover();string child=iterator.Current;
                if(!DiagnosticSafety.Plain(child)){DiscoveryComplete=false;Gap(id,"ReparsePointSkipped");continue;}
                yield return child;
            }
        }
    }
}
