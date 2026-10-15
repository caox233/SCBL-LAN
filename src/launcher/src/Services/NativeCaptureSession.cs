using System.Diagnostics;
using System.IO.Compression;
using System.IO.Pipes;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace SplinterCellCNLauncher.Services;

// Explicit opt-in only. One fatal capture/run, up to four exact process identities,
// 10 minutes, 16 MiB/dump, private ACL, no registry or privilege changes or upload.
internal sealed class NativeCaptureSession : IDisposable
{
    private readonly string root, executable, helper;
    private readonly Action<CaptureObservation> record;
    private readonly int seconds;
    private readonly DateTimeOffset expires;
    internal int[] HelperIds { get { lock(gate)return peers.Where(p=>p.Armed).Select(p=>p.Helper.Id).ToArray(); } }
    private readonly object gate = new();
    private readonly List<Peer> peers = [];
    private bool closed;
    private sealed class Peer(int pid, long creation, string dir, Process helper, NamedPipeServerStream pipe)
    {
        public readonly int Pid = pid;
        public readonly long Creation = creation;
        public readonly string Directory = dir;
        public readonly Process Helper = helper;
        public readonly NamedPipeServerStream Pipe = pipe;
        public string Last = "";
        public bool Armed;
        public DateTimeOffset? CaptureStarted;
    }
    public NativeCaptureSession(string runDirectory, string executable, Action<CaptureObservation> record, string? helperOverride = null, int lifetimeSeconds = 600)
    {
        this.executable = Path.GetFullPath(executable); this.record = record; seconds = Math.Clamp(lifetimeSeconds, 1, 600); expires=DateTimeOffset.UtcNow.AddSeconds(seconds);
        root = Path.Combine(runDirectory, "capture");
        if (!DiagnosticSafety.Plain(root) || !Guid.TryParseExact(Path.GetFileName(runDirectory), "N", out _)) throw new IOException("Invalid capture boundary.");
        Directory.CreateDirectory(root);
        var acl = new DirectorySecurity(); acl.SetAccessRuleProtection(true, false);
        acl.AddAccessRule(new FileSystemAccessRule(WindowsIdentity.GetCurrent().User!, FileSystemRights.FullControl, InheritanceFlags.ContainerInherit | InheritanceFlags.ObjectInherit, PropagationFlags.None, AccessControlType.Allow));
        new DirectoryInfo(root).SetAccessControl(acl);
        helper = helperOverride ?? Path.Combine(root, "SCBL.LAN.Capture.exe");
        if (helperOverride == null) {
            using var input = Assembly.GetExecutingAssembly().GetManifestResourceStream("SCBL.LAN.Capture.exe") ?? throw new IOException("Capture helper resource unavailable.");
            using var output = new FileStream(helper, FileMode.CreateNew, FileAccess.Write, FileShare.Read); input.CopyTo(output);
        }
        record(new(0, DateTimeOffset.UtcNow, CaptureState.Preparing, "Prepare"));
    }
    public void Observe(Process process)
    {
        lock(gate) {
            if(closed || peers.Any(p => p.Pid==process.Id && p.Creation==process.StartTime.ToUniversalTime().ToFileTimeUtc())) return;
            if(DateTimeOffset.UtcNow>=expires){record(new(process.Id,DateTimeOffset.UtcNow,CaptureState.Expired,"Timeout"));return;}
            if(peers.Count>=4) { record(new(process.Id,DateTimeOffset.UtcNow,CaptureState.LimitReached,"Limit"));return; }
            // Observe() in GameRunDiagnostics has already validated this identity; the helper
            // independently checks PID+creation+exact image again, retaining an OS handle.
            long creation=process.StartTime.ToUniversalTime().ToFileTimeUtc();
            string dir=Path.Combine(root,$"{process.Id}-{creation}");Directory.CreateDirectory(dir);
            string name="SCBL.LAN.Capture-"+Guid.NewGuid().ToString("N");
            var pipe=new NamedPipeServerStream(name,PipeDirection.InOut,1,PipeTransmissionMode.Byte,PipeOptions.Asynchronous|PipeOptions.CurrentUserOnly);
            Process? child=null;Peer? peer=null;
            try {
                using var me=Process.GetCurrentProcess();
                var start=new ProcessStartInfo(helper) {UseShellExecute=false,CreateNoWindow=true,WindowStyle=ProcessWindowStyle.Hidden};
                start.ArgumentList.Add(name);start.ArgumentList.Add(me.Id.ToString());start.ArgumentList.Add(me.StartTime.ToUniversalTime().ToFileTimeUtc().ToString());
                child=Process.Start(start) ?? throw new IOException("Capture helper did not start.");
                peer=new(process.Id,creation,dir,child,pipe);peers.Add(peer);
                using var timeout=new CancellationTokenSource(TimeSpan.FromSeconds(5));
                pipe.WaitForConnectionAsync(timeout.Token).GetAwaiter().GetResult();
                if(!GetNamedPipeClientProcessId(pipe.SafePipeHandle,out uint peerPid)||peerPid!=child.Id)throw new IOException("Capture pipe identity mismatch.");
                using var writer=new BinaryWriter(pipe,Encoding.Unicode,true);
                writer.Write(0x53434C43u);writer.Write((uint)process.Id);writer.Write(creation);
                void Text(string text){writer.Write((uint)text.Length);writer.Write(Encoding.Unicode.GetBytes(text));}
                Text(executable);Text(dir);writer.Write((uint)Math.Clamp((int)Math.Ceiling((expires-DateTimeOffset.UtcNow).TotalSeconds),1,seconds));writer.Flush();
                byte[] ack=new byte[4];pipe.ReadExactlyAsync(ack,timeout.Token).AsTask().GetAwaiter().GetResult();
                uint error=BitConverter.ToUInt32(ack);
                if(error!=0){record(new(process.Id,DateTimeOffset.UtcNow,CaptureState.Failed,"Attach",unchecked((int)error)));pipe.Dispose();return;}
                peer.Armed=true;record(new(process.Id,DateTimeOffset.UtcNow,CaptureState.Armed,"Ready"));
                _=Task.Run(()=>Watch(peer));
                child=null; // Watch now owns the helper handle, even if it finishes immediately.
            } catch(Exception e) {
                record(new(process.Id,DateTimeOffset.UtcNow,CaptureState.Failed,"Connect",e.HResult));pipe.Dispose();
                // Before the ready ACK we do not kill a possibly attaching debugger. Closing
                // the pipe asks its bounded loop to detach. It also exits if this launcher dies.
            } finally {
                // Failure before transfer releases only our handle, never the helper or target process.
                child?.Dispose();
            }
        }
    }
    private async Task Watch(Peer p)
    {
        try {
            var deadline=DateTimeOffset.UtcNow.AddSeconds(seconds+10);
            while(!p.Helper.HasExited) {
                PollPeer(p);
                if(DateTimeOffset.UtcNow>deadline || p.CaptureStarted is {} t && DateTimeOffset.UtcNow-t>TimeSpan.FromSeconds(15)) {
                    // ACK proves DebugSetProcessKillOnExit(FALSE) succeeded. Kill only OUR
                    // helper; never a game process. Covers a stuck DbgHelp outside callbacks.
                    p.Helper.Kill();record(new(p.Pid,DateTimeOffset.UtcNow,CaptureState.Failed,"Timeout",1460));break;
                }
                await Task.Delay(250).ConfigureAwait(false);
            }
            PollPeer(p);
            if(p.Helper.HasExited && !new[]{"Captured","Failed","Expired","Detached","Exited","LimitReached"}.Any(s=>p.Last.Contains("\"State\":\""+s+"\"")))
                record(new(p.Pid,DateTimeOffset.UtcNow,CaptureState.Failed,"Helper",p.Helper.ExitCode));
        } catch(ObjectDisposedException) { }
        catch(Exception e){record(new(p.Pid,DateTimeOffset.UtcNow,CaptureState.Failed,"Helper",e.HResult));}
        // Process metadata remains available for local verification; only the helper handle is disposed.
        finally { p.Helper.Dispose(); }
    }
    public void Poll(){lock(gate)foreach(var p in peers)PollPeer(p);}
    private void PollPeer(Peer p)
    {
        lock(p) try {
            string file=Path.Combine(p.Directory,"state.json");
            if(!DiagnosticSafety.Plain(file)||!File.Exists(file)||new FileInfo(file).Length>4096)return;
            string text=File.ReadAllText(file);if(text==p.Last)return;
            using var doc=JsonDocument.Parse(text);var e=doc.RootElement;
            if(e.GetProperty("Pid").GetInt32()!=p.Pid||e.GetProperty("CreatedFileTime").GetInt64()!=p.Creation)return;
            if(!Enum.TryParse<CaptureState>(e.GetProperty("State").GetString(),out var status)||!Enum.IsDefined(status))return;
            string stage=DiagnosticSafety.CaptureStage(e.GetProperty("Stage").GetString()??"");
            if(status==CaptureState.Preparing && stage is "Context" or "Dump")p.CaptureStarted??=DateTimeOffset.UtcNow;
            if(status is CaptureState.Captured or CaptureState.Failed)p.CaptureStarted=null;
            p.Last=text;record(new(p.Pid,e.GetProperty("Utc").GetDateTimeOffset(),status,stage,e.GetProperty("Error").GetInt32()));
        } catch { /* partial atomic update: next poll retries, never claim capture */ }
    }
    public void Dispose()
    {
        lock(gate){if(closed)return;closed=true;foreach(var p in peers){PollPeer(p);p.Pipe.Dispose();}}
        // Helper sees pipe closure and detaches on its debugger thread. Watch remains alive
        // to bound cleanup; no synchronous wait while the UI/config pipe owns a lock.
    }
    [DllImport("kernel32.dll",SetLastError=true)]private static extern bool GetNamedPipeClientProcessId(SafePipeHandle pipe,out uint pid);

    internal static void TrimPrivate(string diagnosticRoot)
    {
        try {
            if(!Directory.Exists(diagnosticRoot)||!DiagnosticSafety.Plain(diagnosticRoot))return;
            var runs=Directory.EnumerateDirectories(diagnosticRoot).Where(d=>Guid.TryParseExact(Path.GetFileName(d),"N",out _)&&DiagnosticSafety.Plain(d)).OrderByDescending(Directory.GetLastWriteTimeUtc).Take(128).ToArray();
            for(int i=0;i<runs.Length;i++) {
                string capture=Path.Combine(runs[i],"capture");if(!Directory.Exists(capture)||!DiagnosticSafety.Plain(capture))continue;
                var age=DateTime.UtcNow-Directory.GetLastWriteTimeUtc(capture);
                if(age<TimeSpan.FromHours(24) && (i<4 || age<TimeSpan.FromMinutes(15)))continue;
                foreach(string child in Directory.EnumerateDirectories(capture).Where(p=>System.Text.RegularExpressions.Regex.IsMatch(Path.GetFileName(p),@"\A[0-9]{1,10}-[0-9]{1,20}\z")).Take(4)) {
                    if(!DiagnosticSafety.Plain(child))continue;
                    foreach(var n in new[]{"exception.dmp","exception.json","state.json","state.tmp"})DeleteOwned(child,n,capture);
                    if(!Directory.EnumerateFileSystemEntries(child).Any())Directory.Delete(child,false);
                }
                foreach(var n in new[]{"fatal.claim","SCBL.LAN.Capture.exe"})DeleteOwned(capture,n,capture);
                if(!Directory.EnumerateFileSystemEntries(capture).Any())Directory.Delete(capture,false);
            }
        } catch { /* An expired file that is locked is unavailable for export and retried later. */ }
    }
    private static void DeleteOwned(string dir,string name,string boundary)
    {
        string file=Path.GetFullPath(Path.Combine(dir,name));
        if(!file.StartsWith(Path.GetFullPath(boundary)+Path.DirectorySeparatorChar,StringComparison.OrdinalIgnoreCase)||!DiagnosticSafety.Plain(file))return;
        if(File.Exists(file))File.Delete(file);
    }
    internal static void ExportPrivate(string runCaptureDirectory,string destination)
    {
        if(!DiagnosticSafety.Plain(runCaptureDirectory)||Path.GetFileName(runCaptureDirectory)!="capture" || !Guid.TryParseExact(Path.GetFileName(Path.GetDirectoryName(runCaptureDirectory)),"N",out _))throw new IOException("Invalid capture boundary.");
        var candidates=Directory.EnumerateDirectories(runCaptureDirectory).Where(p=>System.Text.RegularExpressions.Regex.IsMatch(Path.GetFileName(p),@"\A[0-9]{1,10}-[0-9]{1,20}\z")&&DiagnosticSafety.Plain(p)).Take(4).ToArray();
        string? selected=candidates.FirstOrDefault(p=>File.Exists(Path.Combine(p,"exception.dmp"))&&File.Exists(Path.Combine(p,"exception.json")));
        if(selected==null)throw new IOException("No captured exception exists for this run.");
        string dump=Path.Combine(selected,"exception.dmp"),detail=Path.Combine(selected,"exception.json");
        if(!DiagnosticSafety.Plain(dump)||!DiagnosticSafety.Plain(detail)||new FileInfo(dump).Length>16L*1024*1024||new FileInfo(detail).Length>512*1024||DateTime.UtcNow-File.GetLastWriteTimeUtc(dump)>TimeSpan.FromHours(24))throw new IOException("Capture unavailable, oversized or expired.");
        // Verify the actual ExceptionStream exists, not a post-exit context-free snapshot.
        using(var f=File.OpenRead(dump))using(var reader=new BinaryReader(f)) {
            if(f.Length<32||reader.ReadUInt32()!=0x504d444d)throw new IOException("Invalid minidump.");
            reader.ReadUInt32();uint count=reader.ReadUInt32(),table=reader.ReadUInt32();bool found=false;
            if(count>128||table+(long)count*12>f.Length)throw new IOException("Invalid minidump streams.");
            f.Position=table;for(int i=0;i<count;i++){uint kind=reader.ReadUInt32(),bytes=reader.ReadUInt32(),offset=reader.ReadUInt32();if(kind==6 && bytes>=168 && offset+(long)bytes<=f.Length)found=true;}
            if(!found)throw new IOException("Exception context is missing.");
        }
        string full=Path.GetFullPath(destination);
        if(!full.EndsWith(".zip",StringComparison.OrdinalIgnoreCase)||!DiagnosticSafety.Plain(full))throw new IOException("Invalid export destination.");
        // Publish only a complete private bundle. A failed source read must not leave
        // a final ZIP that looks shareable or prevents retry at the chosen name.
        string temporary=full+"."+Guid.NewGuid().ToString("N")+".tmp";
        bool created=false;
        var expected=new Dictionary<string,(long Bytes,string Sha256)>(StringComparer.Ordinal);
        try {
            if(!DiagnosticSafety.Plain(temporary))throw new IOException("Invalid export temporary path.");
            using(var output=new FileStream(temporary,FileMode.CreateNew,FileAccess.ReadWrite,FileShare.None)) {
                created=true;
                using var zip=new ZipArchive(output,ZipArchiveMode.Create);
                void Add(string name,Stream input) {
                    using var entry=zip.CreateEntry(name).Open();
                    using var hash=System.Security.Cryptography.IncrementalHash.CreateHash(System.Security.Cryptography.HashAlgorithmName.SHA256);
                    byte[] buffer=new byte[16384];long bytes=0;int read;
                    while((read=input.Read(buffer,0,buffer.Length))!=0){entry.Write(buffer,0,read);hash.AppendData(buffer,0,read);bytes+=read;}
                    expected.Add(name,(bytes,Convert.ToHexString(hash.GetHashAndReset())));
                }
                using(var input=File.OpenRead(dump))Add("exception.dmp",input);
                using(var input=File.OpenRead(detail))Add("exception.json",input);
                using var notice=new MemoryStream(Encoding.UTF8.GetBytes("Explicitly opted-in private exception capture. May contain sensitive stack data despite minimization. Review before sharing. No automatic upload. Not part of the ordinary diagnostic ZIP."));
                Add("PRIVACY.txt",notice);
            }
            if(!DiagnosticSafety.Plain(temporary))throw new IOException("Invalid export temporary path.");
            using(var zip=ZipFile.OpenRead(temporary)) {
                if(zip.Entries.Count!=expected.Count||!zip.Entries.Select(e=>e.FullName).ToHashSet(StringComparer.Ordinal).SetEquals(expected.Keys))throw new IOException("Private export entry mismatch.");
                foreach(var entry in zip.Entries) {
                    var payload=expected[entry.FullName];
                    if(entry.Length!=payload.Bytes)throw new IOException("Private export size mismatch.");
                    using var input=entry.Open();
                    if(Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(input))!=payload.Sha256)throw new IOException("Private export hash mismatch.");
                }
            }
            if(!DiagnosticSafety.Plain(full)||!DiagnosticSafety.Plain(temporary))throw new IOException("Invalid export destination.");
            File.Move(temporary,full,false);
            created=false;
        } finally {
            // Never remove a pre-existing destination or a CreateNew collision.
            if(created)try{if(DiagnosticSafety.Plain(temporary)&&File.Exists(temporary))File.Delete(temporary);}
                catch(Exception e) when(e is IOException or UnauthorizedAccessException){ /* Preserve the original export failure. */ }
        }
    }
}
