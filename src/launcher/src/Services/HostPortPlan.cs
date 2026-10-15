using System.Net;
using System.Net.Sockets;

namespace SplinterCellCNLauncher.Services;

/// <summary>All production ports remain fixed. Never work around a bind failure by changing ports.</summary>
public sealed record HostPortPlan(int ApiPort, string Notice)
{

    public static HostPortPlan Create(string ip, bool smoke = false) => Create(ip, smoke, Probe, PortOwnerDiagnostics.Describe);

    internal static HostPortPlan Create(string ip, bool smoke, Action<IPAddress, int, ProtocolType> probe,
        Func<int, ProtocolType, string>? describe = null)
    {
        var address = IPAddress.Parse(ip);
        foreach (int port in smoke ? new[] { 18080, 18000 } : new[] { 80, 8000 })
            Required(address, port, ProtocolType.Tcp, probe, describe);
        foreach (int port in smoke ? new[] { 22126, 22127 } : new[] { 21126, 21127 })
            Required(address, port, ProtocolType.Udp, probe, describe);

        int apiPort = smoke ? 15051 : 50051;
        Required(address, apiPort, ProtocolType.Tcp, probe, describe);
        return new(apiPort, "");
    }

    private static void Required(IPAddress address, int port, ProtocolType protocol, Action<IPAddress, int, ProtocolType> probe,
        Func<int, ProtocolType, string>? describe)
    {
        try { probe(address, port, protocol); }
        catch (SocketException e) { throw Explain(address, port, protocol, e, describe?.Invoke(port,protocol) ?? ""); }
    }

    private static IOException Explain(IPAddress address, int port, ProtocolType protocol, SocketException e, string owner)
    {
        string reason = e.SocketErrorCode switch
        {
            SocketError.AccessDenied => "Windows 拒绝绑定；可能是端口保留、独占占用或安全软件限制，并不等同于需要管理员权限",
            SocketError.AddressAlreadyInUse => "端口已被占用",
            SocketError.AddressNotAvailable => "所选 IP 已不属于本机，请重新选择网卡",
            _ => e.Message
        };
        return new IOException($"房主服务无法绑定 {protocol.ToString().ToUpperInvariant()} {address}:{port}（Winsock {e.ErrorCode}）：{reason}。" +
                               "端口保持不变；请检查占用和系统限制后重试。启动器不会更换端口或终止其他程序。"+owner, e);
    }

    private static void Probe(IPAddress address, int port, ProtocolType protocol)
    {
        using var socket = new Socket(AddressFamily.InterNetwork,
            protocol == ProtocolType.Tcp ? SocketType.Stream : SocketType.Dgram, protocol);
        socket.ExclusiveAddressUse = true;
        socket.Bind(new IPEndPoint(address, port));
    }
}
