using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Globalization;

namespace SplinterCellCNLauncher.Services;

// Retains every line, not just recognized events. Known-pattern minimization, NOT a universal secret detector.
internal static class DiagnosticTextSanitizer
{
    private static readonly UTF8Encoding Utf8=new(false,true);
    private static Regex Rule(string pattern)=>new(pattern,RegexOptions.IgnoreCase|RegexOptions.CultureInvariant|RegexOptions.NonBacktracking,TimeSpan.FromMilliseconds(50));
    private static readonly Regex Assignment=Rule("\\b(?:password|passwd|token|authorization|cookie|access_key|crypto_key|ticket_key|username|account_id|accountid|user_id|userid|cdkeys|signature|reset_code|recovery_code)\\\"?\\s*[:=]\\s*(?:\\\"(?:\\\\.|[^\\\"\\\\])*\\\"|'[^']*'|[^,;\\r\\n]+)");
    private static readonly Regex Bearer=Rule(@"\bBearer\s+[A-Za-z0-9._~+/-]+=*");
    private static readonly Regex UriCredentials=Rule(@"https?://[^\s/@]+:[^\s/@]+@[^\s]+" );
    private static readonly Regex Paths=Rule(@"(?:[A-Za-z]:[\\/]|\\\\)[^\r\n\""<>|]*");
    private static readonly Regex Email=Rule(@"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b");
    private static readonly Regex IdentityMessage=Rule(@"(?:Login successful for|New user|Password reset completed for|Verify plain password of|Verify password hash of)\s+[^\r\n]*");
    private static readonly HashSet<string> SecretKeys=new(StringComparer.OrdinalIgnoreCase){"password","passwd","token","authorization","cookie","access_key","crypto_key","ticket_key","username","account_id","accountid","user_id","userid","cdkeys","signature","reset_code","recovery_code"};
    internal static string Scrub(string text,ref int count)
    {
        foreach(var rule in new[]{Assignment,Bearer,UriCredentials,Paths,Email,IdentityMessage})
        {int n=0;text=rule.Replace(text,_=>{n++;return "[REDACTED]";});count=checked(count+n);}
        return text;
    }
    internal static byte[] Line(ReadOnlySpan<byte> bytes,DiagnosticExportContext context,ref int redactions)
    {
        context.Check();string original;try{original=Utf8.GetString(bytes);}catch(DecoderFallbackException){throw new InvalidDataException("Unsupported UTF-8 diagnostic text; no archive published.");}
        string end=original.EndsWith("\r\n",StringComparison.Ordinal)?"\r\n":original.EndsWith('\n')?"\n":"";
        string body=end.Length==0?original:original[..^end.Length];int count=0;
        string trimmed=body.TrimStart('\uFEFF',' ','\t');
        bool launcherPrefix=trimmed.Length>=21&&trimmed[0]=='['&&trimmed[20]==']'&&DateTime.TryParseExact(trimmed.Substring(1,19),"yyyy-MM-dd HH:mm:ss",CultureInfo.InvariantCulture,DateTimeStyles.None,out _);
        string arrayStart=trimmed.StartsWith('[')?trimmed[1..].TrimStart():"";
        bool arrayJson=!launcherPrefix&&arrayStart.Length>0&&("{[\"0123456789-tfn]".Contains(arrayStart[0]));
        if(trimmed.StartsWith('{')||arrayJson)
        {
            try
            {
                using var doc=JsonDocument.Parse(body.TrimStart('\uFEFF'),new(){MaxDepth=context.Budget.MaxJsonDepth});
                using var output=new MemoryStream();using(var writer=new Utf8JsonWriter(output))
                {int nodes=0;Write(doc.RootElement,writer,context,ref nodes,ref count);}
                if(count>0)body=Utf8.GetString(output.ToArray());
            }
            catch(JsonException){throw new InvalidDataException("Unsupported JSON log line or depth limit; no archive published.");}
        }
        else body=Scrub(body,ref count);
        redactions=checked(redactions+count);return Utf8.GetBytes(body+end);
    }
    private static void Write(JsonElement e,Utf8JsonWriter writer,DiagnosticExportContext context,ref int nodes,ref int count)
    {
        context.Check();if(++nodes>context.Budget.MaxJsonNodes)throw new InvalidDataException("Diagnostic JSON node limit exceeded.");
        switch(e.ValueKind)
        {
            case JsonValueKind.Object:
                writer.WriteStartObject();foreach(var p in e.EnumerateObject())
                {writer.WritePropertyName(p.Name);if(SecretKeys.Contains(p.Name)){writer.WriteStringValue("[REDACTED]");count++;}else Write(p.Value,writer,context,ref nodes,ref count);}writer.WriteEndObject();break;
            case JsonValueKind.Array:
                writer.WriteStartArray();foreach(var item in e.EnumerateArray())Write(item,writer,context,ref nodes,ref count);writer.WriteEndArray();break;
            case JsonValueKind.String:writer.WriteStringValue(Scrub(e.GetString()!,ref count));break;
            default:e.WriteTo(writer);break;
        }
    }
    internal static void Copy(FileStream input,Stream output,long length,DiagnosticExportSource source,DiagnosticExportContext context)
    {
        byte[] chunk=new byte[16384],line=new byte[context.Budget.MaxLineBytes];int pending=0,sourceRedactions=source.Redactions;long left=length;
        var projected=new Queue<DiagnosticSourceProjection.Event>();
        while(left>0)
        {
            context.Check();int read=input.ReadAsync(chunk.AsMemory(0,(int)Math.Min(left,chunk.Length)),context.Token).AsTask().GetAwaiter().GetResult();
            if(read==0)throw new IOException("Diagnostic source truncated while reading.");context.Input(read);source.ReadBytes+=read;left-=read;
            for(int i=0;i<read;i++)
            {
                if(pending==line.Length)throw new InvalidDataException("Diagnostic line limit exceeded; no archive published.");
                line[pending++]=chunk[i];if(chunk[i]=='\n'){Emit();}
            }
        }
        if(pending>0)Emit();context.Projection(source,projected);
        void Emit()
        {
            byte[] sanitized=Line(line.AsSpan(0,pending),context,ref sourceRedactions);
            string original=Utf8.GetString(line,0,pending).TrimEnd('\r','\n');
            if(original.Length<=8192)
            {
                source.UnknownFields+=DiagnosticSourceProjection.CountUnknownFields(original);
                var e=DiagnosticSourceProjection.Project(source.Id,original);
                if(e==null)source.Dropped++;
                else{source.EventsRecognized++;projected.Enqueue(e);if(projected.Count>256){projected.Dequeue();source.Dropped++;}}
            }
            else source.Dropped++;
            context.Check();source.Redactions=sourceRedactions;context.Output(sanitized.Length);output.WriteAsync(sanitized,context.Token).AsTask().GetAwaiter().GetResult();source.ExportBytes+=sanitized.Length;source.Lines++;pending=0;
        }
    }
}
