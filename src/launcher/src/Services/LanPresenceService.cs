using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using ScblLan;

namespace SplinterCellCNLauncher.Services;

public sealed record LanPlayer(string Instance, string Name, string Address, string Mode, bool InGame, double? RttMs);
public sealed record LanPresenceReply(string Kind, string Nonce, string Session, LanPlayer[] Players);
public sealed record LanPresenceQuery(string Kind, string Nonce, string Instance, int ApiPort, string Name, string Mode, bool InGame, double? RttMs);
public sealed record LanPresenceSnapshot(bool Replied, LanPlayer[] Players, LanQualitySnapshot Quality);

// Reuses the existing UDP 47777 discovery listener; no new fixed port, VPN integration, database or account enumeration.
public sealed class LanPresenceRegistry
{
    public const int Port = 47777;
    public const int MaxBytes = 4096;
    private readonly Dictionary<string, (LanPlayer Player, DateTime Seen)> players = new();
    private readonly Dictionary<string, DateTime> lastRequest = new();
    private string session = "";
    internal Func<DateTime> Clock { get; set; } = () => DateTime.UtcNow;
    internal static bool Id(string? value) => value is { Length: 32 } && value.All(Uri.IsHexDigit);
    internal static string Clean(string? value) => new((value ?? "").Where(c => !char.IsControl(c) && c != '\u202e').Take(32).ToArray());
    internal static bool ValidPlayer(LanPlayer p) => Id(p.Instance) && p.Name is { Length: > 0 and <= 32 } && p.Name == Clean(p.Name)
        && IPAddress.TryParse(p.Address, out var ip) && LanConnection.IsUnicast(ip) && p.Mode is "Official" or "MOD"
        && (p.RttMs == null || p.RttMs is >= 0 and <= 60000 && double.IsFinite(p.RttMs.Value));
    public byte[]? Handle(byte[] data, IPEndPoint remote, string hostSession, string hostName, string hostIp, int apiPort, string hostMode, bool inGame)
    {
        if (data.Length != MaxBytes || !LanConnection.IsUnicast(remote.Address) || !Id(hostSession)
            || !IPAddress.TryParse(hostIp, out var hostAddress) || !LanConnection.IsUnicast(hostAddress) || hostMode is not ("Official" or "MOD")) return null;
        LanPresenceQuery? q;
        try { q = JsonSerializer.Deserialize<LanPresenceQuery>(data, new JsonSerializerOptions { MaxDepth = 8 }); }
        catch (JsonException) { return null; }
        if (q == null || q.Kind != "SCBL_LAN_STATUS_QUERY_V1" || !Id(q.Nonce) || !Id(q.Instance) || q.ApiPort != apiPort) return null;
        var peer = new LanPlayer(q.Instance, Clean(q.Name), remote.Address.ToString(), q.Mode, q.InGame, q.RttMs);
        if (!ValidPlayer(peer)) return null;
        lock (players)
        {
            if (session != hostSession) { players.Clear(); lastRequest.Clear(); session = hostSession; }
            DateTime now = Clock();
            foreach (var key in players.Where(p => now - p.Value.Seen > TimeSpan.FromSeconds(15)).Select(p => p.Key).ToArray()) players.Remove(key);
            foreach (var key in lastRequest.Where(p => now - p.Value > TimeSpan.FromSeconds(15)).Select(p => p.Key).ToArray()) lastRequest.Remove(key);
            string keyId = remote.Address + "/" + q.Instance;
            if (lastRequest.TryGetValue(keyId, out var previous) && now - previous < TimeSpan.FromMilliseconds(400)) return null;
            if (!players.ContainsKey(keyId) && players.Count >= 15) return null;
            lastRequest[keyId] = now;
            // The host querying itself is represented only once, using its stable launcher instance ID.
            players[keyId] = (peer, now);
            var host = new LanPlayer(hostSession, Clean(hostName) is { Length: > 0 } name ? name : "Host", hostIp, hostMode, inGame, 0);
            var rows = new[] { host }.Concat(players.Values.Select(p => p.Player).Where(p => p.Address != hostIp)).Take(16).ToArray();
            byte[] response;
            do
            {
                response = JsonSerializer.SerializeToUtf8Bytes(new LanPresenceReply("SCBL_LAN_STATUS_REPLY_V1", q.Nonce, hostSession, rows));
                if (response.Length <= data.Length) return response; // Never amplify the request.
                rows = rows.Take(rows.Length - 1).ToArray();
            } while (rows.Length > 0);
            return null;
        }
    }
}

public sealed class LanPresenceClient
{
    private readonly int port, timeoutMs;
    private readonly string instance = Guid.NewGuid().ToString("N");
    private readonly LanQualityWindow quality = new();
    private string scope = "", hostSession = "";
    private LanPlayer[] lastPlayers = [];
    private DateTime lastReply;
    public LanPresenceClient(int port = LanPresenceRegistry.Port, int timeoutMs = 1500) { this.port = port; this.timeoutMs = timeoutMs; }
    public static byte[] Query(string nonce, string instance, int apiPort, string name, string mode, bool inGame, double? rtt)
    {
        var q = new LanPresenceQuery("SCBL_LAN_STATUS_QUERY_V1", nonce, instance, apiPort, LanPresenceRegistry.Clean(name), mode, inGame, rtt);
        byte[] json = JsonSerializer.SerializeToUtf8Bytes(q), padded = Enumerable.Repeat((byte)' ', LanPresenceRegistry.MaxBytes).ToArray();
        if (json.Length > padded.Length) throw new ArgumentException("Presence request too large.");
        json.CopyTo(padded, 0); return padded;
    }
    public async Task<LanPresenceSnapshot> ProbeAsync(string address, string bindIp, string name, string mode, bool inGame, CancellationToken ct = default)
    {
        string newScope = address.Trim().ToLowerInvariant() + "|" + bindIp;
        if (scope != newScope) { scope = newScope; quality.Clear(); lastPlayers = []; hostSession = ""; lastReply = default; }
        try
        {
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(ct); deadline.CancelAfter(timeoutMs);
            var target = ServerAddress.Parse(address);
            var ip = (await Dns.GetHostAddressesAsync(target.Host, AddressFamily.InterNetwork, deadline.Token)).First(LanConnection.IsUnicast);
            var local = LanConnection.SelectSource(ip, target.Port, bindIp);
            using var udp = new UdpClient(new IPEndPoint(local, 0));
            string nonce = Guid.NewGuid().ToString("N");
            byte[] request = Query(nonce, instance, target.Port, string.IsNullOrWhiteSpace(name) ? "Player" : name, mode, inGame, quality.Snapshot().RttMs);
            var timer = Stopwatch.StartNew();
            await udp.SendAsync(request, new IPEndPoint(ip, port), deadline.Token);
            while (true)
            {
                var received = await udp.ReceiveAsync(deadline.Token);
                if (!received.RemoteEndPoint.Address.Equals(ip) || received.Buffer.Length > LanPresenceRegistry.MaxBytes) continue;
                LanPresenceReply? reply;
                try { reply = JsonSerializer.Deserialize<LanPresenceReply>(received.Buffer, new JsonSerializerOptions { MaxDepth = 8 }); }
                catch (JsonException) { continue; }
                if (reply == null || reply.Kind != "SCBL_LAN_STATUS_REPLY_V1" || reply.Nonce != nonce || !LanPresenceRegistry.Id(reply.Session)
                    || reply.Players == null || reply.Players.Length > 16 || reply.Players.Any(p => p == null || !LanPresenceRegistry.ValidPlayer(p))) continue;
                if (hostSession != reply.Session) { hostSession = reply.Session; quality.Clear(); }
                quality.Add(timer.Elapsed.TotalMilliseconds); lastPlayers = reply.Players; lastReply = DateTime.UtcNow;
                return new(true, lastPlayers, quality.Snapshot());
            }
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested) { throw; }
        catch (Exception e) when (e is SocketException or OperationCanceledException or IOException or ArgumentException or InvalidOperationException)
        {
            quality.Add(null);
            if (DateTime.UtcNow - lastReply > TimeSpan.FromSeconds(15)) lastPlayers = [];
            return new(false, lastPlayers, quality.Snapshot());
        }
    }
}
