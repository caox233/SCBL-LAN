using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace SplinterCellCNLauncher.Services;

// Safe projection only. Never retains the raw LaunchPipe frame or any credential-derived hash.
internal sealed record RequestedConfigurationProjection(DateTimeOffset PreparedUtc,JsonElement? Values,int Redactions,string? ErrorCode);
internal sealed class RequestedConfigurationSchemaException(string code):JsonException("Requested configuration snapshot rejected.")
{ internal string Code {get;}=code; }
internal static class RequestedConfigurationSnapshot
{
    internal const string FileName="effective-config.redacted.json";
    internal const int MaxBytes=65536;
    private static readonly string[] Fields=["ConfigServer","ApiServer","GameplayMode","EnableOverlay","AutoJoinInvite","User.Username","User.Password","User.AccountId","User.CdKeys","Save.SaveDir","ForwardCalls","ForwardAllCalls","InternalCommandLine","EnableHooks","EnableAllHooks","Networking.IpAddress","Networking.Adapter","Logging.Level","Features.FovFactor","Release.Schema","Release.PackageRoot","Release.ReleaseId","Release.CompatId","Release.ManifestSha256"];
    private static readonly string[] Envelope=["Schema","RunId","ConfigurationKind","Source","PreparedUtc","RecordedUtc","NativeLoadedValues","HostAssociation","Values","NotSpecifiedInFrame","Redactions","UnknownFields","InvalidValues"];
    private static readonly UTF8Encoding Utf8=new(false,true);
    private static DiagnosticExportBudget Budget=>new(){MaxStructuredBytes=MaxBytes,MaxFileInputBytes=MaxBytes,MaxTotalInputBytes=MaxBytes,MaxOutputBytes=MaxBytes,MaxJsonDepth=8,MaxJsonNodes=1024,DeadlineMilliseconds=1000};
    internal static string SafeError(string? code)=>new[]{"ProjectionInvalid","ProjectionBudget","ProjectionUnknownField","ProjectionInvalidValue","IOError","ReparsePoint","Conflict","InvalidBoundary"}.Contains(code)?code!:"ProjectionInvalid";
    internal static RequestedConfigurationProjection Prepare(byte[] frame)
    {
        var utc=DateTimeOffset.UtcNow;
        try {
            if(frame.Length>MaxBytes)return new(utc,null,0,"ProjectionBudget");
            using var context=new DiagnosticExportContext(Budget,CancellationToken.None);
            var projected=DiagnosticConfigurationExport.ProjectHookFrame(frame,context);
            if(projected.UnknownFields!=0)return new(utc,null,0,"ProjectionUnknownField");
            if(projected.InvalidValues!=0)return new(utc,null,0,"ProjectionInvalidValue");
            if(JsonSerializer.SerializeToUtf8Bytes(projected.Values).Length>MaxBytes)return new(utc,null,0,"ProjectionBudget");
            return new(utc,projected.Values,projected.Redactions,null);
        } catch(Exception e) {return new(utc,null,0,e is InvalidDataException or OperationCanceledException?"ProjectionBudget":"ProjectionInvalid");}
    }
    private static string[] Missing(JsonElement values)=>Fields.Where(path=>!Present(values,path)).ToArray();
    private static bool Present(JsonElement values,string path)
    {
        foreach(string key in path.Split('.')){if(values.ValueKind!=JsonValueKind.Object||!values.TryGetProperty(key,out var next))return false;values=next;}return true;
    }
    private static byte[] Serialize(Guid id,DateTimeOffset prepared,DateTimeOffset recorded,JsonElement values,int redactions)=>JsonSerializer.SerializeToUtf8Bytes(new {
        Schema=1,RunId=id,ConfigurationKind="RequestedLaunchConfiguration",Source="LaunchPipeFrame",PreparedUtc=prepared,RecordedUtc=recorded,
        NativeLoadedValues="NotObserved",HostAssociation="EndpointOnlyNoHostInstanceEvidence",Values=values,NotSpecifiedInFrame=Missing(values),Redactions=redactions,UnknownFields=0,InvalidValues=0});
    internal static RequestedConfigurationStatus WriteOnce(string directory,Guid id,RequestedConfigurationProjection projection)
    {
        string? temporary=null;bool created=false;
        try {
            if(id==Guid.Empty||Path.GetFileName(directory)!=id.ToString("N")||Path.GetFileName(Path.GetDirectoryName(directory))!="diagnostics-v2")return new("WriteFailed",ErrorCode:"InvalidBoundary");
            string final=Path.Combine(directory,FileName);
            if(!DiagnosticSafety.Plain(final))return new("ReparsePointSkipped",ErrorCode:"ReparsePoint");
            if(File.Exists(final)||Directory.Exists(final))return new("Conflict",ErrorCode:"Conflict");
            if(projection.Values is not JsonElement values)return new("ProjectionUnavailable",ErrorCode:SafeError(projection.ErrorCode));
            DateTimeOffset recorded=DateTimeOffset.UtcNow;
            byte[] bytes=Serialize(id,projection.PreparedUtc,recorded,values,projection.Redactions);
            if(bytes.Length>MaxBytes)return new("ProjectionUnavailable",ErrorCode:"ProjectionBudget");
            using(var check=new DiagnosticExportContext(Budget,CancellationToken.None))Validate(bytes,id,check);
            temporary=Path.Combine(directory,FileName+"."+Guid.NewGuid().ToString("N")+".tmp");
            if(!DiagnosticSafety.Plain(temporary))return new("ReparsePointSkipped",ErrorCode:"ReparsePoint");
            using(var output=new FileStream(temporary,FileMode.CreateNew,FileAccess.Write,FileShare.None)){created=true;output.Write(bytes);output.Flush(true);}
            // Reopen exactly our safe temporary. No raw config has ever been written.
            using(var input=new FileStream(temporary,FileMode.Open,FileAccess.Read,FileShare.Read)) {
                if(input.Length!=bytes.Length)throw new IOException();
                byte[] reopened=new byte[bytes.Length];input.ReadExactly(reopened);
                if(!reopened.AsSpan().SequenceEqual(bytes))throw new IOException();
            }
            if(!DiagnosticSafety.Plain(final)||!DiagnosticSafety.Plain(temporary))return new("ReparsePointSkipped",ErrorCode:"ReparsePoint");
            try {File.Move(temporary,final,false);}catch(IOException) when(File.Exists(final)||Directory.Exists(final)){return new("Conflict",ErrorCode:"Conflict");}
            return new("Written",1,bytes.Length,Convert.ToHexString(SHA256.HashData(bytes)),recorded);
        } catch(Exception e){return new("WriteFailed",ErrorCode:e is RequestedConfigurationSchemaException or JsonException?"ProjectionInvalid":"IOError");}
        finally {if(created&&temporary!=null)try{if(DiagnosticSafety.Plain(temporary)&&File.Exists(temporary))File.Delete(temporary);}catch(Exception){ /* Never delete a competing or reparse file, or affect launch. */ }}
    }
    internal static byte[] Read(byte[] input,DiagnosticExportSource source,DiagnosticExportContext context)
    {
        string[] pieces=source.Id.Split(':');
        if(pieces.Length!=3||!Guid.TryParseExact(pieces[1],"N",out var id))throw new RequestedConfigurationSchemaException("SnapshotRunIdMismatch");
        // Require a successfully read, unchanged metadata role from this exact run, not the most recent run.
        if(!context.SnapshotRuns.TryGetValue(id,out var metadata))throw new RequestedConfigurationSchemaException("SnapshotMetadataMissingOrUnavailable");
        if(metadata?.State!="Written")throw new RequestedConfigurationSchemaException("SnapshotMetadataNotWritten");
        var result=Validate(input,id,context);
        if(input.LongLength!=metadata.Bytes||!Convert.ToHexString(SHA256.HashData(input)).Equals(metadata.Sha256,StringComparison.OrdinalIgnoreCase))throw new RequestedConfigurationSchemaException("SnapshotMetadataMismatch");
        source.Redactions=result.Redactions;
        return Serialize(id,result.PreparedUtc,result.RecordedUtc,result.Values,result.Redactions);
    }
    private sealed record Validated(DateTimeOffset PreparedUtc,DateTimeOffset RecordedUtc,JsonElement Values,int Redactions);
    private static Validated Validate(byte[] input,Guid id,DiagnosticExportContext context)
    {
        if(input.Length>MaxBytes)throw new InvalidDataException("Requested snapshot input limit exceeded.");
        try{_ = Utf8.GetString(input);}catch(DecoderFallbackException){throw new RequestedConfigurationSchemaException("SnapshotInvalidUtf8");}
        var reader=new Utf8JsonReader(input,new JsonReaderOptions{MaxDepth=9});int nodes=0;
        while(reader.Read()) {
            context.Check();
            if(reader.TokenType is not(JsonTokenType.PropertyName or JsonTokenType.EndObject or JsonTokenType.EndArray)&&++nodes>1024)throw new InvalidDataException("Requested snapshot node limit exceeded.");
            if(reader.TokenType is JsonTokenType.StartObject or JsonTokenType.StartArray && reader.CurrentDepth>=8)throw new InvalidDataException("Requested snapshot depth limit exceeded.");
        }
        using var document=JsonDocument.Parse(input,new(){MaxDepth=8});JsonElement root=document.RootElement;
        void Unique(JsonElement element) {
            context.Check();
            if(element.ValueKind==JsonValueKind.Object){var names=new HashSet<string>(StringComparer.Ordinal);foreach(var item in element.EnumerateObject()){if(!names.Add(item.Name))throw new RequestedConfigurationSchemaException("SnapshotDuplicateKey");Unique(item.Value);}}
            else if(element.ValueKind==JsonValueKind.Array)foreach(var child in element.EnumerateArray())Unique(child);
        }
        Unique(root);
        if(root.ValueKind!=JsonValueKind.Object)throw new RequestedConfigurationSchemaException("SnapshotInvalidSchema");
        if(root.EnumerateObject().Any(p=>!Envelope.Contains(p.Name))||Envelope.Any(p=>!root.TryGetProperty(p,out _)))throw new RequestedConfigurationSchemaException("SnapshotUnknownOrMissingField");
        if(root.GetProperty("Schema").GetInt32()!=1)throw new RequestedConfigurationSchemaException("SnapshotUnsupportedSchema");
        if(root.GetProperty("RunId").GetGuid()!=id)throw new RequestedConfigurationSchemaException("SnapshotRunIdMismatch");
        if(root.GetProperty("ConfigurationKind").GetString()!="RequestedLaunchConfiguration"||root.GetProperty("Source").GetString()!="LaunchPipeFrame"||root.GetProperty("NativeLoadedValues").GetString()!="NotObserved"||root.GetProperty("HostAssociation").GetString()!="EndpointOnlyNoHostInstanceEvidence")throw new RequestedConfigurationSchemaException("SnapshotInvalidSemantics");
        var prepared=root.GetProperty("PreparedUtc").GetDateTimeOffset();var recorded=root.GetProperty("RecordedUtc").GetDateTimeOffset();
        var projected=DiagnosticConfigurationExport.ProjectHookValues(root.GetProperty("Values"),context);
        if(projected.UnknownFields!=0)throw new RequestedConfigurationSchemaException("SnapshotUnknownValueField");
        if(projected.InvalidValues!=0)throw new RequestedConfigurationSchemaException("SnapshotInvalidValue");
        // Already-redacted schema must be semantically canonical, not merely named redacted.
        // DeepEquals ignores JSON property order; raw secrets/free-form aliases cannot pass.
        if(!JsonElement.DeepEquals(root.GetProperty("Values"),projected.Values))throw new RequestedConfigurationSchemaException("SnapshotNoncanonicalRedaction");
        var missing=root.GetProperty("NotSpecifiedInFrame");
        if(missing.ValueKind!=JsonValueKind.Array||!missing.EnumerateArray().Select(x=>x.GetString()).SequenceEqual(Missing(projected.Values)))throw new RequestedConfigurationSchemaException("SnapshotMissingFieldMismatch");
        if(root.GetProperty("Redactions").GetInt32()!=projected.Redactions||root.GetProperty("UnknownFields").GetInt32()!=0||root.GetProperty("InvalidValues").GetInt32()!=0)throw new RequestedConfigurationSchemaException("SnapshotCounterMismatch");
        return new(prepared,recorded,projected.Values,projected.Redactions);
    }
}
