using System.Net;
using System.Net.Http;
using System.Net.NetworkInformation;
using System.Net.Sockets;
using Grpc.Net.Client;
using SplinterCellCNLauncher.Network;

namespace ScblLan;

public sealed record ServerAddress(string Host, int Port)
{
    public static ServerAddress Parse(string text)
    {
        text = text.Trim();
        if (text.Length == 0) throw new ArgumentException("请填写服务器 IPv4 或域名。");
        if (!text.Contains("://")) text = "http://" + text;
        if (!Uri.TryCreate(text, UriKind.Absolute, out var u) || u.Scheme != "http" || u.UserInfo.Length > 0
            || u.AbsolutePath != "/" || u.Query.Length > 0 || u.Fragment.Length > 0 || u.HostNameType == UriHostNameType.IPv6)
            throw new ArgumentException("请输入 IPv4 或域名，可附加 API 端口；首版使用可信 LAN 上的直连 HTTP。");
        bool explicitPort = text[(text.IndexOf("://", StringComparison.Ordinal) + 3)..].TrimEnd('/').Contains(':');
        int port = explicitPort ? u.Port : 50051;
        if (port < 1) throw new ArgumentException("端口必须在 1–65535 范围内。");
        return new(u.Host, port);
    }
}

public sealed class LanConnection : IDisposable
{
    // Retained wire authentication identifier for existing LAN accounts; gameplay mode is a separate field.
    public const string ProductContract = "scbl-lan/1/official";
    public IPAddress ServerIp { get; }
    public IPAddress LocalIp { get; }
    public int Port { get; }
    public CapabilitiesResponse Capabilities { get; private set; } = new();
    public Users.UsersClient Api { get; }
    private readonly GrpcChannel channel;
    private readonly HttpClient http;
    private LanConnection(IPAddress server, IPAddress local, int port)
    {
        ServerIp = server; LocalIp = local; Port = port;
        var handler = new SocketsHttpHandler { UseProxy = false, AllowAutoRedirect = false, ConnectTimeout = TimeSpan.FromSeconds(5) };
        handler.ConnectCallback = async (context, ct) =>
        {
            var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
            try
            {
                socket.Bind(new IPEndPoint(LocalIp, 0));
                await socket.ConnectAsync(ServerIp, context.DnsEndPoint.Port, ct);
                return new NetworkStream(socket, ownsSocket: true);
            }
            catch { socket.Dispose(); throw; }
        };
        http = new HttpClient(handler) { Timeout = TimeSpan.FromSeconds(8) };
        channel = GrpcChannel.ForAddress($"http://{ServerIp}:{port}", new GrpcChannelOptions { HttpClient = http });
        Api = new(channel);
    }
    public static async Task<LanConnection> ConnectAsync(string address, string bindOverride, CancellationToken ct)
    {
        ServerAddress target = ServerAddress.Parse(address);
        var addresses = await Dns.GetHostAddressesAsync(target.Host, AddressFamily.InterNetwork, ct);
        Exception? last = null;
        foreach (var ip in addresses.Where(IsUnicast))
        {
            LanConnection? result = null;
            try
            {
                var local = SelectSource(ip, target.Port, bindOverride);
                result = new(ip, local, target.Port);
                await result.RefreshAsync(ct);
                return result;
            }
            catch (Exception e)
            {
                result?.Dispose();
                if (e is OperationCanceledException) throw;
                last = e;
            }
        }
        throw new IOException("无法连接兼容的 SCBL LAN 服务端。检查地址、服务器、防火墙及路由。" + (last is null ? "没有可用 IPv4。" : " " + last.Message), last);
    }
    public static bool IsUnicast(IPAddress ip) => ip.AddressFamily == AddressFamily.InterNetwork && ip.GetAddressBytes()[0] is > 0 and < 224;
    public static IPAddress SelectSource(IPAddress server, int port, string overrideIp)
    {
        using var route = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
        if (!string.IsNullOrWhiteSpace(overrideIp))
        {
            if (!IPAddress.TryParse(overrideIp.Trim(), out var manual) || !IsUnicast(manual)) throw new ArgumentException("绑定地址必须是本机 IPv4。");
            route.Bind(new IPEndPoint(manual, 0)); // OS rejects addresses not assigned locally.
        }
        route.Connect(server, port); // Route selection only; sends no packet.
        var source = ((IPEndPoint)route.LocalEndPoint!).Address;
        if (!IsUnicast(source)) throw new IOException("系统未选择有效本地 IPv4。");
        return source;
    }
    public async Task RefreshAsync(CancellationToken ct)
    {
        var c = await Api.GetCapabilitiesAsync(new(), deadline: DateTime.UtcNow.AddSeconds(6), cancellationToken: ct);
        if (c.Product != "SCBL LAN" || c.ProtocolVersion != 1) throw new IOException("服务端与当前版本协议不兼容。");
        if (!c.Ready) throw new IOException("服务端尚未就绪：必要 TCP/UDP 监听未全部建立。");
        foreach (uint port in new[] { c.ConfigPort, c.ContentPort, c.AuthPort, c.SecurePort })
            if (port == 0 || port > 65535) throw new IOException("服务端返回无效端口。");
        using var config = await http.GetAsync($"http://{ServerIp}:{c.ConfigPort}/", ct);
        config.EnsureSuccessStatusCode();
        using var content = await http.GetAsync($"http://{ServerIp}:{c.ContentPort}/", ct);
        if (content.StatusCode != HttpStatusCode.NotFound && !content.IsSuccessStatusCode) throw new IOException("内容服务响应异常。");
        Capabilities = c;
    }
    public void AssertRouteUnchanged(string overrideIp)
    {
        if (!SelectSource(ServerIp, Port, overrideIp).Equals(LocalIp)) throw new IOException("本机路由或地址已变化，请重新连接服务器。");
    }
    public void Dispose() { channel.Dispose(); http.Dispose(); }
}
