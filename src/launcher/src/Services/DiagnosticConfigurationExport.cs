using System.Globalization;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using Tomlyn;
using Tomlyn.Model;
using Tomlyn.Parsing;

namespace SplinterCellCNLauncher.Services;

// Fixed producer schemas only. Never resolves paths or opens files from a configuration value.
internal sealed class DiagnosticConfigurationExport(DiagnosticExportContext context)
{
    private const string Secret="[REDACTED_SECRET]",Unknown="[REDACTED_UNKNOWN]",Invalid="[REDACTED_INVALID]";
    private static readonly UTF8Encoding Utf8=new(false,true);
    private static readonly string[] ServiceNames=["onlineconfig","content","sc_bl_secure","sc_bl_auth"];
    private static readonly string[] Hooks=["Printer","LeaveState","NextState","NetResultBase","Goal","SetStep","Thread","ChangeState","NetCore","NetResultCore","NetResultSession","NetResultRdvSession","NetResultLobby","StormHostPortToString","GetAdaptersInfo","Gethostbyname","GenerateID","StormSetState","StormStateMachineActionExecute","StormErrorFormatter","GearStrDestructor","StormEventDispatcher","StormPackets","RMCMessages","OverridePackaged"];
    private int redactions,unknownFields,invalidValues,parsedNodes;
    private object Mask(string value=Secret){redactions++;return value;}
    private object Bad(){invalidValues++;return Mask(Invalid);}
    private object Bool(JsonElement e)=>e.ValueKind is JsonValueKind.True or JsonValueKind.False?e.GetBoolean():Bad();
    private object Number(JsonElement e,double min,double max)=>e.ValueKind==JsonValueKind.Number&&e.TryGetDouble(out double n)&&double.IsFinite(n)&&n>=min&&n<=max?n:Bad();
    private object Integer(JsonElement e,int min,int max)=>e.ValueKind==JsonValueKind.Number&&e.TryGetInt32(out int n)&&n>=min&&n<=max?n:Bad();
    private object Choice(JsonElement e,params string[] values)=>e.ValueKind==JsonValueKind.String&&values.Contains(e.GetString(),StringComparer.Ordinal)?e.GetString()!:Bad();
    private object Ip(JsonElement e)=>e.ValueKind==JsonValueKind.String&&Address(e.GetString()) is string ip?ip:Bad();
    private object Socket(JsonElement e)=>e.ValueKind==JsonValueKind.String&&Endpoint(e.GetString()) is string endpoint?endpoint:Bad();
    private object Executable(JsonElement e)=>e.ValueKind==JsonValueKind.String&&e.GetString()=="Blacklist_DX11_game.exe"?"Blacklist_DX11_game.exe":Mask("<GameExecutable>");
    private object Sensitive(JsonElement e)=>Mask();
    private object PathValue(JsonElement e)=>Mask("<GameRoot>");
    private object[] Array(JsonElement e,Func<JsonElement,object> convert)
    {
        if(e.ValueKind!=JsonValueKind.Array){Bad();return [];}
        var result=new List<object>();foreach(var item in e.EnumerateArray()){context.Check();result.Add(convert(item));}return result.ToArray();
    }
    private Dictionary<string,object> Fields(JsonElement e,params (string Key,Func<JsonElement,object> Convert)[] fields)
    {
        if(e.ValueKind!=JsonValueKind.Object)throw new JsonException();
        var values=new Dictionary<string,object>(StringComparer.Ordinal);
        foreach(var p in e.EnumerateObject())
        {
            context.Check();var f=fields.FirstOrDefault(f=>f.Key==p.Name);
            if(f.Key==null){unknownFields++;redactions++;continue;}
            if(!values.TryAdd(p.Name,f.Convert(p.Value)))throw new JsonException(); // Duplicate JSON keys are rejected, not collapsed.
        }
        return values;
    }
    internal static byte[] Project(byte[] input,DiagnosticExportSource source,DiagnosticExportContext context)
    {
        var result=ProjectCore(input,source.Kind,context);
        source.Redactions=result.Redactions;source.UnknownFields=result.UnknownFields;source.InvalidValues=result.InvalidValues;
        return JsonSerializer.SerializeToUtf8Bytes(new{Schema=1,ConfigurationKind=source.Kind,StoredConfigurationOnly=true,Values=result.Values,result.Redactions,result.UnknownFields,result.InvalidValues});
    }
    internal sealed record HookProjection(JsonElement Values,int Redactions,int UnknownFields,int InvalidValues);
    internal static HookProjection ProjectHookFrame(byte[] input,DiagnosticExportContext context)=>ProjectCore(input,"RequestedFrame",context);
    internal static HookProjection ProjectHookValues(JsonElement values,DiagnosticExportContext context)
    {
        var parser=new DiagnosticConfigurationExport(context);parser.Nodes(values);
        var safe=parser.HookConfig(values);return new(JsonSerializer.SerializeToElement(safe),parser.redactions,parser.unknownFields,parser.invalidValues);
    }
    private static HookProjection ProjectCore(byte[] input,string kind,DiagnosticExportContext context)
    {
        var parser=new DiagnosticConfigurationExport(context);string text;
        try{text=Utf8.GetString(input);}catch(DecoderFallbackException){throw new JsonException();}
        int line=0;foreach(byte b in input){context.Check();if(b==10)line=0;else if(++line>context.Budget.MaxLineBytes)throw new InvalidDataException("Diagnostic configuration line limit exceeded.");}
        object values;
        if(kind=="ConfigBalancing")values=parser.Ini(text);
        else
        {
            JsonElement root;
            if(kind is "ConfigLauncher" or "ConfigLegacy")
            {parser.JsonBudget(input);using var document=JsonDocument.Parse(input,new(){MaxDepth=context.Budget.MaxJsonDepth});root=document.RootElement.Clone();}
            else
            {
                var options=new TomlSerializerOptions{MaxDepth=context.Budget.MaxJsonDepth};
                try
                {
                    // Library grammar, not a bracket-counting regex. Bound events before AST materialization.
                    // One extra library level lets our budget exception remain distinct from syntax errors.
                    var scan=TomlParser.Create(text,new TomlSerializerOptions{MaxDepth=context.Budget.MaxJsonDepth+1});int events=0;
                    while(scan.MoveNext()){context.Check();if(scan.Depth>context.Budget.MaxJsonDepth)throw new InvalidDataException("Diagnostic TOML depth limit exceeded.");if(++events>checked(context.Budget.MaxJsonNodes*4+8))throw new InvalidDataException("Diagnostic TOML event limit exceeded.");}
                    // TomlTable's converter is last-wins even with DuplicateKeyHandling.Error.
                    // The same library's semantic syntax validator rejects duplicate keys/tables.
                    context.Check();SyntaxParser.ParseStrict(text,options);context.Check();
                    context.Check();var model=TomlSerializer.Deserialize<TomlTable>(text,options)??throw new JsonException();context.Check();
                    root=JsonSerializer.SerializeToElement(model); // Private bounded intermediate, never a payload.
                }
                catch(TomlException){throw new JsonException();}
            }
            parser.Nodes(root);
            values=kind switch
            {
                "ConfigLauncher"=>parser.Launcher(root),"ConfigLegacy"=>parser.Legacy(root),
                "ConfigReference"=>parser.Reference(root),"ConfigService"=>parser.Service(root),
                "ConfigLegacyHooks" or "RequestedFrame"=>parser.HookConfig(root),_=>throw new JsonException()
            };
        }
        return new(JsonSerializer.SerializeToElement(values),parser.redactions,parser.unknownFields,parser.invalidValues);
    }
    private void Nodes(JsonElement root)
    {
        Visit(root);void Visit(JsonElement e){context.Check();if(++parsedNodes>context.Budget.MaxJsonNodes)throw new InvalidDataException("Diagnostic configuration node limit exceeded.");if(e.ValueKind==JsonValueKind.Object)foreach(var p in e.EnumerateObject())Visit(p.Value);else if(e.ValueKind==JsonValueKind.Array)foreach(var item in e.EnumerateArray())Visit(item);}
    }
    private void JsonBudget(ReadOnlySpan<byte> bytes)
    {
        var reader=new Utf8JsonReader(bytes,new JsonReaderOptions{MaxDepth=context.Budget.MaxJsonDepth+1});int nodes=0;
        while(reader.Read()){context.Check();if(reader.TokenType is not(JsonTokenType.PropertyName or JsonTokenType.EndObject or JsonTokenType.EndArray)&&++nodes>context.Budget.MaxJsonNodes)throw new InvalidDataException("Diagnostic JSON configuration node limit exceeded.");if(reader.TokenType is JsonTokenType.StartObject or JsonTokenType.StartArray&&reader.CurrentDepth>=context.Budget.MaxJsonDepth)throw new InvalidDataException("Diagnostic JSON configuration depth limit exceeded.");}
    }
    private object Launcher(JsonElement e)=>Fields(e,("Username",Sensitive),("Password",Sensitive),("ServerAddress",Server),
        ("GameDirectory",PathValue),("GameExecutable",Executable),("GameplayMode",v=>Choice(v,"Official","MOD")),("FovScale",v=>Number(v,1,1.3)),
        ("RememberPassword",Bool),("CredentialStorageVersion",v=>Integer(v,0,100000)),("BindOverride",OptionalIp),("LastBindIp",OptionalIp),
        ("SaveOverwritePromptHandled",Bool),("Language",v=>Choice(v,"zh-CN","en-US")),("MusicEnabled",Bool),("GuideCompleted",Bool));
    private object Legacy(JsonElement e)=>Fields(e,("Server",Server),("GameExecutable",Executable),("BindOverride",OptionalIp),("Username",Sensitive));
    private object OptionalIp(JsonElement e)=>e.ValueKind==JsonValueKind.String&&e.GetString()==""?"":Ip(e);
    private object Server(JsonElement e)
    {
        if(e.ValueKind!=JsonValueKind.String)return Bad();string value=e.GetString()!;
        if(value=="")return "";if(Address(value) is string ip)return ip;
        if(Endpoint(value) is string socket)return socket;
        if(Uri.TryCreate(value,UriKind.Absolute,out var uri)&&uri.Scheme is "http" or "https"&&uri.UserInfo==""&&uri.AbsolutePath=="/"&&uri.Query==""&&uri.Fragment==""&&Address(uri.Host) is string host&&uri.Port is >0 and <=65535)return uri.Scheme+"://"+Host(host)+":"+uri.Port;
        return Bad();
    }
    private object Reference(JsonElement e)=>Fields(e,("LaunchPipe",v=>
        v.ValueKind==JsonValueKind.String&&v.GetString() is string s&&s.StartsWith("scbl-lan-",StringComparison.Ordinal)&&Guid.TryParseExact(s[9..],"N",out _)?Mask("[REDACTED_SESSION_REFERENCE]"):Bad()));
    private object Service(JsonElement e)=>Fields(e,("api_server",Socket),("debug",v=>Fields(v,("mark_all_as_online",Bool),("force_joins",Bool))),
        ("services",v=>Array(v,Name)),("service",ServiceMap));
    private object Name(JsonElement e)=>e.ValueKind==JsonValueKind.String&&ServiceNames.Contains(e.GetString(),StringComparer.Ordinal)?e.GetString()!:Mask(Unknown);
    private object ServiceMap(JsonElement e)
    {
        if(e.ValueKind!=JsonValueKind.Object)throw new JsonException();var values=new Dictionary<string,object>();int slot=0;
        foreach(var p in e.EnumerateObject())
        {context.Check();string name=ServiceNames.Contains(p.Name,StringComparer.Ordinal)?p.Name:"Service_"+(++slot).ToString(CultureInfo.InvariantCulture);if(name!=p.Name){unknownFields++;redactions++;}values.Add(name,Variant(p.Value));}return values;
    }
    private object Variant(JsonElement e)
    {
        if(e.ValueKind!=JsonValueKind.Object)throw new JsonException();string? type=null;bool inferred=false;
        if(e.TryGetProperty("type",out var t)){if(t.ValueKind==JsonValueKind.String&&new[]{"authentication","secure","config","content"}.Contains(t.GetString()))type=t.GetString();else return Bad();}
        else{inferred=true;type=e.TryGetProperty("secure_server_addr",out var addr)&&addr.ValueKind!=JsonValueKind.Null?"authentication":"secure";}
        Dictionary<string,object> values=type switch
        {
            "authentication" or "secure"=>Fields(e,("type",v=>Choice(v,"authentication","secure")),("access_key",Sensitive),("crypto_key",Sensitive),("ticket_key",Sensitive),
                ("listen",Socket),("vport",v=>Integer(v,0,255)),("secure_server_addr",v=>v.ValueKind==JsonValueKind.Null?null!:Socket(v)),("settings",SettingsMap)),
            "config"=>Fields(e,("type",v=>Choice(v,"config")),("listen",Socket),("content",Online)),
            "content"=>Fields(e,("type",v=>Choice(v,"content")),("listen",Socket),("files",Files)),_=>throw new JsonException()
        };
        if(inferred)values.Add("InferredLegacyType",type!);return values;
    }
    private object SettingsMap(JsonElement e)=>Fields(e,("storage_host",Socket),("storage_path",v=>v.ValueKind==JsonValueKind.String&&v.GetString()=="/mp_balancing.ini"?"/mp_balancing.ini":Mask("<StoragePath>")));
    private object Files(JsonElement e)=>Fields(e,("/mp_balancing.ini",v=>v.ValueKind==JsonValueKind.String&&v.GetString() is "data/mp_balancing.ini" or "./data/mp_balancing.ini"?"<DataRoot>/data/mp_balancing.ini":Mask("<ContentPath>")));
    private object Online(JsonElement e)
    {
        string form="Typed";JsonElement array=e;
        if(e.ValueKind==JsonValueKind.String)
        {form="RawJson";string text=e.GetString()!;if(Utf8.GetByteCount(text)>context.Budget.MaxStructuredBytes)throw new InvalidDataException("Diagnostic nested configuration input limit exceeded.");JsonBudget(Utf8.GetBytes(text));using var doc=JsonDocument.Parse(text,new(){MaxDepth=context.Budget.MaxJsonDepth});array=doc.RootElement.Clone();Nodes(array);}
        if(array.ValueKind!=JsonValueKind.Array)throw new JsonException();var rows=new List<object>();
        foreach(var item in array.EnumerateArray())
        {
            context.Check();if(item.ValueKind!=JsonValueKind.Object)throw new JsonException();string? name=item.TryGetProperty("Name",out var n)&&n.ValueKind==JsonValueKind.String?n.GetString():null;
            bool known=name is "SandboxUrl" or "SandboxUrlWS";
            rows.Add(Fields(item,("Name",v=>known?name!:Mask(Unknown)),("Values",v=>Array(v,x=>known?(name=="SandboxUrl"?Prudp(x):Socket(x)):Mask(Unknown)))));
        }
        return new{InputForm=form,Items=rows};
    }
    private object Prudp(JsonElement e)
    {
        if(e.ValueKind!=JsonValueKind.String)return Bad();string s=e.GetString()!;const string prefix="prudp:/address=";if(!s.StartsWith(prefix,StringComparison.Ordinal))return Bad();
        string[] pieces=s[prefix.Length..].Split(';');if(pieces.Length!=2||!pieces[1].StartsWith("port=",StringComparison.Ordinal)||Address(pieces[0]) is not string ip||!int.TryParse(pieces[1][5..],NumberStyles.None,CultureInfo.InvariantCulture,out int port)||port is <1 or >65535)return Bad();return prefix+ip+";port="+port;
    }
    private object ReleaseId(JsonElement e)=>e.ValueKind==JsonValueKind.String&&e.GetString() is string s&&s.Length is >0 and <=64&&s.All(c=>char.IsAsciiLetterOrDigit(c)||c is '.' or '_' or '-')?s:Bad();
    private object ReleaseDigest(JsonElement e)=>e.ValueKind==JsonValueKind.String&&e.GetString() is string s&&s.Length==64&&s.All(c=>c is >='0' and <='9' or >='a' and <='f')?s:Bad();
    private object Release(JsonElement e)=>Fields(e,("Schema",v=>Integer(v,1,1)),("PackageRoot",v=>Mask("<PackageRoot>")),("ReleaseId",ReleaseId),("CompatId",ReleaseId),("ManifestSha256",ReleaseDigest));
    private object HookConfig(JsonElement e)=>Fields(e,("Release",Release),("User",v=>Fields(v,("Username",Sensitive),("Password",Sensitive),("CdKeys",Sensitive),("AccountId",Sensitive))),
        ("Save",v=>Fields(v,("SaveDir",SaveDirectory))),("ForwardCalls",v=>Array(v,x=>Choice(x,"UPLAY_Startup","UPLAY_Quit"))),("ForwardAllCalls",Bool),("InternalCommandLine",Sensitive),
        ("EnableHooks",v=>Array(v,h=>Choice(h,Hooks))),("EnableAllHooks",Bool),("EnableOverlay",Bool),("ConfigServer",Server),("ApiServer",Server),
        ("Networking",v=>Fields(v,("IpAddress",OptionalIp),("Adapter",Sensitive))),("Logging",v=>Fields(v,("Level",x=>Choice(x,"TRACE","DEBUG","INFO","WARNING","ERROR")))),
        ("AutoJoinInvite",Bool),("GameplayMode",v=>Choice(v,"Official","MOD")),("Features",v=>Fields(v,("FovFactor",x=>Number(x,1,1.3)))));
    private object SaveDirectory(JsonElement e)=>e.ValueKind==JsonValueKind.String&&e.GetString()=="Roaming"?"Roaming":e.ValueKind==JsonValueKind.Object?Fields(e,("Custom",v=>Mask("<SaveDirectory>"))):Bad();
    private object Ini(string text)
    {
        string section="";int comments=0,nodes=0;var rows=new List<object>();
        var keys=new Dictionary<string,string[]>(StringComparer.Ordinal){["Echelon.EPlayerPawn"]=["SpyWeaponEffectDurationLookUpTable"],["Echelon.GI_Adv_AntiGrenadeSystem"]=["m_AdversarialInventoryLimit"],["MPArmorHelmetTech"]=["m_fBMSRefreshDelayMultiplier"],
            ["EXPLOSION_STICKYCAM_ADV"]=["GroundZeroRadius","GroundZeroZoneDamage","BlastZoneDamage","NPCGroundZeroRadius","NPCBlastRadius","NPCGroundZeroZoneDamage","NPCBlastZoneDamage"],["EXPLOSION_PROXMINE_ADV"]=["GroundZeroZoneDamage"],["EXPLOSION_GRENADE_ADV"]=["GroundZeroZoneDamage"],["EXPLOSION_ADVDRONE"]=["GroundZeroZoneDamage"]};
        using var reader=new StringReader(text);string? line;
        while((line=reader.ReadLine())!=null)
        {
            context.Check();line=line.Trim().TrimStart('\uFEFF');if(line=="")continue;if(++nodes>context.Budget.MaxJsonNodes)throw new InvalidDataException("Diagnostic INI node limit exceeded.");
            if(line.StartsWith(';')||line.StartsWith('#')){comments++;redactions++;continue;}
            if(line.StartsWith('[')){if(!line.EndsWith(']')||line.Length<3)throw new JsonException();section=line[1..^1];if(!keys.ContainsKey(section)){unknownFields++;redactions++;}continue;}
            int eq=line.IndexOf('=');if(eq<=0||section=="")throw new JsonException();string key=line[..eq].Trim(),literal=line[(eq+1)..].Trim();
            bool known=keys.TryGetValue(section,out var allowed)&&allowed.Contains(key,StringComparer.Ordinal);
            if(!known){unknownFields++;redactions++;rows.Add(new{Section=keys.ContainsKey(section)?section:Unknown,Key=Unknown,Literal=Unknown,Number=(double?)null});continue;}
            var match=Regex.Match(literal,@"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?[fF]?;?$",RegexOptions.CultureInvariant|RegexOptions.NonBacktracking,TimeSpan.FromMilliseconds(50));
            double? number=match.Success&&double.TryParse(literal.TrimEnd(';').TrimEnd('f','F'),NumberStyles.Float,CultureInfo.InvariantCulture,out double value)&&double.IsFinite(value)?value:null;
            if(number==null){Bad();literal=Invalid;}
            rows.Add(new{Section=section,Key=key,Literal=literal,Number=number});
        }
        return new{Rows=rows,CommentsRedacted=comments};
    }
    private static string? Address(string? value)
    {if(value==null||value.Length>64||value.Contains('%')||!IPAddress.TryParse(value.Trim('[',']'),out var ip)||ip.AddressFamily is not(AddressFamily.InterNetwork or AddressFamily.InterNetworkV6))return null;return ip.ToString();}
    private static string Host(string ip)=>ip.Contains(':')?"["+ip+"]":ip;
    private static string? Endpoint(string? value)
    {
        if(value==null||value.Length>80)return null;int split=value.LastIndexOf(':');if(split<1||!int.TryParse(value[(split+1)..],NumberStyles.None,CultureInfo.InvariantCulture,out int port)||port is <1 or >65535)return null;
        string raw=value[..split];if(raw.Contains(':')&&!(raw.StartsWith('[')&&raw.EndsWith(']')))return null;return Address(raw) is string ip?Host(ip)+":"+port:null;
    }
}
