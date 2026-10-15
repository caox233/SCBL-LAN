using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace SplinterCellCNLauncher.Services;

// Persistent JSON is untrusted, including files called .redacted. No arbitrary strings or unknown field names leave this schema.
internal static class DiagnosticStructuredExport
{
    internal static string? IPv4(string? value)=>value?.Length<=15 && IPAddress.TryParse(value,out var ip)&&ip.AddressFamily==AddressFamily.InterNetwork?ip.ToString():null;
    internal static string Endpoint(string? value)
    {
        if(value==null||value.Length>64)return "";
        try{var a=ScblLan.ServerAddress.Parse(value);string? host=IPv4(a.Host);return host!=null&&a.Port is >0 and <=65535?host+":"+a.Port:"";}catch(ArgumentException){return "";}
    }
    private static string? Hash(string? s)=>s?.Length==64&&s.All(Uri.IsHexDigit)?s.ToUpperInvariant():null;
    private static string KnownModule(string? s)=>DiagnosticSafety.Modules.Contains(s,StringComparer.OrdinalIgnoreCase)?DiagnosticSafety.Modules.First(m=>m.Equals(s,StringComparison.OrdinalIgnoreCase)):"UnknownModule";
    private static string? Version(string? s)=>s?.Length<=48?DiagnosticSafety.Version(s):null;
    internal static byte[] Read(FileStream stream,DiagnosticExportSource source,DiagnosticExportContext context)
    {
        if(stream.Length>context.Budget.MaxStructuredBytes)throw new InvalidDataException("Diagnostic structured input limit exceeded.");
        byte[] input=new byte[checked((int)stream.Length)];int filled=0;
        while(filled<input.Length){context.Check();int n=stream.ReadAsync(input.AsMemory(filled),context.Token).AsTask().GetAwaiter().GetResult();if(n==0)throw new IOException("Diagnostic source truncated.");context.Input(n);source.ReadBytes+=n;filled+=n;}
        if(source.Kind=="EffectiveConfigSnapshot")return RequestedConfigurationSnapshot.Read(input,source,context);
        if(source.Kind.StartsWith("Config",StringComparison.Ordinal))return DiagnosticConfigurationExport.Project(input,source,context);
        using var document=JsonDocument.Parse(input,new(){MaxDepth=context.Budget.MaxJsonDepth});int nodes=0;Count(document.RootElement);
        void Count(JsonElement e){context.Check();if(++nodes>context.Budget.MaxJsonNodes)throw new InvalidDataException("Diagnostic JSON node limit exceeded.");if(e.ValueKind==JsonValueKind.Object)foreach(var p in e.EnumerateObject())Count(p.Value);else if(e.ValueKind==JsonValueKind.Array)foreach(var v in e.EnumerateArray())Count(v);}
        if(source.Kind=="CaptureState")return Capture(document.RootElement,source);
        var run=JsonSerializer.Deserialize<RunSummary>(input,DiagnosticSafety.Json)??throw new JsonException();
        if(run.Schema!=2||run.RunId==Guid.Empty||!source.Id.StartsWith("run:"+run.RunId.ToString("N")+":",StringComparison.Ordinal))throw new JsonException();
        // Invalid enums are schema failures, never affirmative evidence invented by a fallback.
        if(!Enum.IsDefined(run.Intent)||!Enum.IsDefined(run.Outcome)||run.LaunchProfile==null||!Enum.IsDefined(run.LaunchProfile.Mode)||!Enum.IsDefined(run.LaunchProfile.RequestedSource))throw new JsonException();
        if((run.LoadedModules??[]).Any(m=>m==null||!Enum.IsDefined(m.Source)||!Enum.IsDefined(m.HashState)) ||
            (run.Stages??[]).Any(e=>e==null||!Enum.IsDefined(e.Stage)) || (run.Capture??[]).Any(e=>e==null||!Enum.IsDefined(e.State)) ||
            (run.Components??[]).Any(e=>e==null||!Enum.IsDefined(e.Status)) || (run.Processes??[]).Any(p=>p==null||!Enum.IsDefined(p.Evidence)||!Enum.IsDefined(p.WerState)))throw new JsonException();
        if(document.RootElement.TryGetProperty("Processes",out var rawProcesses)&&rawProcesses.ValueKind==JsonValueKind.Array)
            foreach(var process in rawProcesses.EnumerateArray())if(process.ValueKind!=JsonValueKind.Object||!process.TryGetProperty("Evidence",out _))throw new JsonException();
        source.SnapshotMetadata=DiagnosticSafety.Clean(run.EffectiveConfigurationSnapshot);
        // A supplied digest may cover forged non-redacted input. Keep it only for internal binding.
        // ZIP payload hashes, not untrusted source digests, prove the exported safe bytes.
        run.EffectiveConfigurationSnapshot=source.SnapshotMetadata is {} snapshot ? snapshot with {Sha256=null} : null;
        run.Executable="Blacklist_DX11_game.exe";run.ExecutableSha256=Hash(run.ExecutableSha256);run.ExecutablePathSha256=Hash(run.ExecutablePathSha256);
        run.LaunchProfile=new(run.LaunchProfile.Mode,"DX11",Version(run.LaunchProfile.Generation)??"0.4.0",run.LaunchProfile.RequestedSource);
        run.LoadedModulesState=new[]{"NotObserved","Observed","Partial","Unavailable"}.Contains(run.LoadedModulesState)?run.LoadedModulesState:"Unavailable";
        run.LoadedModules=(run.LoadedModules??[]).Select(m=>m with{ Name=KnownModule(m.Name),Sha256=Hash(m.Sha256)}).ToList();
        run.Stages=(run.Stages??[]).Select(DiagnosticSafety.CleanStage).ToList();
        run.Capture=(run.Capture??[]).Select(e=>e with{Stage=DiagnosticSafety.CaptureStage(e.Stage)}).ToList();
        run.LauncherEvents=(run.LauncherEvents??[]).Select(DiagnosticSafety.Clean).ToList();
        run.Components=(run.Components??[]).Select(e=>e with{Name=KnownModule(e.Name),Sha256=Hash(e.Sha256),Version=Version(e.Version)}).ToList();
        run.Processes??=[];
        foreach(var p in run.Processes)
        {
            p.Faults=(p.Faults??[]).Select(e=>{
                if(DiagnosticSafety.FaultRejectionReason(e,p.Pid)!=null)source.InvalidValues=checked(source.InvalidValues+1);
                // Full roles retain the safe record and its fixed rejection reason, not an affirmative match.
                // Keep the full-export module allowlist stricter than the legacy aggregate module projection.
                return DiagnosticSafety.CleanFault(e with{ModuleVersion=Version(e.ModuleVersion)},p.Pid,KnownModule(e.Module));
            }).ToList();
        }
        // Counts describe rejected unknown property names without ever echoing them.
        var known=JsonSerializer.SerializeToElement(run,DiagnosticSafety.Json);int unknown=0;Unknown(document.RootElement,known);source.UnknownFields=unknown;
        void Unknown(JsonElement raw,JsonElement safe)
        {
            if(raw.ValueKind==JsonValueKind.Object&&safe.ValueKind==JsonValueKind.Object)foreach(var p in raw.EnumerateObject())
            {if(!safe.TryGetProperty(p.Name,out var s))unknown++;else Unknown(p.Value,s);}
            else if(raw.ValueKind==JsonValueKind.Array&&safe.ValueKind==JsonValueKind.Array){int i=0;foreach(var r in raw.EnumerateArray()){if(i<safe.GetArrayLength())Unknown(r,safe[i++]);}}
        }
        return JsonSerializer.SerializeToUtf8Bytes(run,DiagnosticSafety.Json);
    }
    private static byte[] Capture(JsonElement item,DiagnosticExportSource source)
    {
        string child=source.Id[(source.Id.LastIndexOf(':')+1)..];string[] identity=child.Split('-');
        int pid=item.GetProperty("Pid").GetInt32();long created=item.GetProperty("CreatedFileTime").GetInt64();
        if(identity.Length!=2||identity[0]!=pid.ToString(System.Globalization.CultureInfo.InvariantCulture)||identity[1]!=created.ToString(System.Globalization.CultureInfo.InvariantCulture))throw new JsonException();
        if(!Enum.TryParse<CaptureState>(item.GetProperty("State").GetString(),out var state)||!Enum.IsDefined(state))throw new JsonException();
        var value=new{Pid=pid,CreatedFileTime=created,Utc=item.GetProperty("Utc").GetDateTimeOffset(),State=state.ToString(),Stage=DiagnosticSafety.CaptureStage(item.GetProperty("Stage").GetString()??""),Error=item.GetProperty("Error").GetInt32()};
        source.UnknownFields=item.EnumerateObject().Count(p=>p.Name is not("Pid" or "CreatedFileTime" or "Utc" or "State" or "Stage" or "Error"));
        return JsonSerializer.SerializeToUtf8Bytes(value);
    }
    internal static DiagnosticEvents.Entry[] Session()=>DiagnosticEvents.Snapshot().Select(e=>{var safe=DiagnosticSafety.Clean(new LauncherRunEvent(e.Utc,e.Level,e.Code,e.ExceptionType,e.HResult,e.SocketError));return new DiagnosticEvents.Entry(safe.Utc,safe.Level,safe.Code,safe.ExceptionType,safe.HResult,safe.SocketError);}).ToArray();
    internal static DiagnosticSourceProjection.Config ConfigSummary(string root,string? hostBind,DiagnosticExportContext context)
    {
        int? hostApiPort=HostApiPort(root,hostBind,context);
        var empty=new DiagnosticSourceProjection.Config(null,null,null,null,null,null,hostApiPort);string path=Path.Combine(root,"launcher_settings.json");
        try
        {
            context.Check();if(!DiagnosticSafety.Plain(path)||!File.Exists(path))return empty;
            using var f=new FileStream(path,FileMode.Open,FileAccess.Read,FileShare.ReadWrite|FileShare.Delete,16384,FileOptions.Asynchronous);
            if(f.Length>context.Budget.MaxStructuredBytes||f.Length>context.Budget.MaxFileInputBytes)throw new InvalidDataException("Diagnostic config input limit exceeded.");
            byte[] bytes=new byte[(int)f.Length];int at=0;while(at<bytes.Length){context.Check();int n=f.ReadAsync(bytes.AsMemory(at),context.Token).AsTask().GetAwaiter().GetResult();if(n==0)return empty;context.Input(n);at+=n;}
            using var d=JsonDocument.Parse(bytes,new(){MaxDepth=context.Budget.MaxJsonDepth});int nodes=0;ValidateNodes(d.RootElement);void ValidateNodes(JsonElement item){context.Check();if(++nodes>context.Budget.MaxJsonNodes)throw new InvalidDataException("Diagnostic config node limit exceeded.");if(item.ValueKind==JsonValueKind.Object)foreach(var property in item.EnumerateObject())ValidateNodes(property.Value);else if(item.ValueKind==JsonValueKind.Array)foreach(var child in item.EnumerateArray())ValidateNodes(child);}var e=d.RootElement;if(e.ValueKind!=JsonValueKind.Object)return empty;
            string? mode=e.TryGetProperty("GameplayMode",out var m)&&m.ValueKind==JsonValueKind.String?GameplayRuntime.Normalize(m.GetString()):null;
            double? fov=e.TryGetProperty("FovScale",out var v)&&v.TryGetDouble(out double fovValue)&&double.IsFinite(fovValue)?GameplayRuntime.NormalizeFov(fovValue):null;
            bool? remember=e.TryGetProperty("RememberPassword",out var r)&&r.ValueKind is JsonValueKind.True or JsonValueKind.False?r.GetBoolean():null;
            string? bind=e.TryGetProperty("BindOverride",out var b)&&b.ValueKind==JsonValueKind.String?IPv4(b.GetString()):null;
            string endpoint=e.TryGetProperty("ServerAddress",out var s)&&s.ValueKind==JsonValueKind.String?Endpoint(s.GetString()):"";string[] pieces=endpoint.Split(':');
            return new(mode,fov,remember,bind,endpoint.Length>0?pieces[0]:null,endpoint.Length>0?int.Parse(pieces[1],System.Globalization.CultureInfo.InvariantCulture):null,hostApiPort);
        }
        catch(Exception e)when((e is IOException && e is not InvalidDataException) || e is UnauthorizedAccessException or JsonException or InvalidOperationException){return empty;}
    }
    // Only the previous safe api_server port summary, not a TOML export or path discovered from its contents.
    private static int? HostApiPort(string root,string? hostBind,DiagnosticExportContext context)
    {
        string? host=IPv4(hostBind);if(host==null)return null;
        string path=Path.Combine(root,"server",host.Replace('.','_'),"service.toml");
        try
        {
            context.Check();if(!DiagnosticSafety.Plain(path)||!File.Exists(path))return null;
            using var input=new FileStream(path,FileMode.Open,FileAccess.Read,FileShare.ReadWrite|FileShare.Delete,16384,FileOptions.Asynchronous);
            if(input.Length>context.Budget.MaxStructuredBytes||input.Length>context.Budget.MaxFileInputBytes)throw new InvalidDataException("Diagnostic config input limit exceeded.");
            byte[] bytes=new byte[(int)input.Length];int at=0;
            while(at<bytes.Length){context.Check();int n=input.ReadAsync(bytes.AsMemory(at),context.Token).AsTask().GetAwaiter().GetResult();if(n==0)throw new IOException("Diagnostic config truncated.");context.Input(n);at+=n;}
            var utf8=new UTF8Encoding(false,true);int start=0;int? port=null;
            for(int end=0;end<=bytes.Length;end++)
            {
                context.Check();if(end<bytes.Length&&bytes[end]!=10){if(end-start+1>context.Budget.MaxLineBytes)throw new InvalidDataException("Diagnostic config line limit exceeded.");continue;}
                string line=utf8.GetString(bytes.AsSpan(start,end-start));start=end+1;
                var match=Regex.Match(line,"^api_server\\s*=\\s*\"([0-9.]+):([0-9]{1,5})\"\\s*$",RegexOptions.CultureInvariant|RegexOptions.NonBacktracking,TimeSpan.FromMilliseconds(50));
                if(match.Success&&IPv4(match.Groups[1].Value)!=null&&int.TryParse(match.Groups[2].Value,out int value)&&value is >0 and <=65535)port=value;
            }
            return port;
        }
        catch(Exception e)when((e is IOException&&e is not InvalidDataException)||e is UnauthorizedAccessException or DecoderFallbackException){return null;}
    }
}
