using System.Net;
using System.Net.NetworkInformation;
using System.Net.Sockets;
using SplinterCellCNLauncher.Models;
using ScblLan;
namespace SplinterCellCNLauncher.Services;
public sealed class NetworkAdapterService
{
    public List<BindIpCandidate> GetBindIpCandidates() => NetworkInterface.GetAllNetworkInterfaces()
        .Where(n => n.OperationalStatus == OperationalStatus.Up)
        .SelectMany(n => n.GetIPProperties().UnicastAddresses.Where(a => LanConnection.IsUnicast(a.Address) && !IPAddress.IsLoopback(a.Address))
            .Select(a => new BindIpCandidate { AdapterName=n.Name, Description=n.Description, IpAddress=a.Address.ToString(), Reason="本机 IPv4；不代表远端 UDP 已互通" }))
        .OrderBy(a=>a.AdapterName,StringComparer.Ordinal).ThenBy(a=>a.IpAddress,StringComparer.Ordinal).ToList();
    public BindIpCandidate? Select(string manual, string server, bool hosting)
    {
        var all=GetBindIpCandidates();
        if (!string.IsNullOrWhiteSpace(manual))
            return all.FirstOrDefault(a=>a.IpAddress==manual) ?? throw new IOException("所选网卡地址已失效，请重新选择。");
        if (hosting)
        {
            if(all.Count>1) throw new IOException("房主模式检测到多个本机地址，请先明确选择供玩家访问的网卡。");
            return all.SingleOrDefault();
        }
        if (!string.IsNullOrWhiteSpace(server))
        {
            var endpoint=ServerAddress.Parse(server);
            // DNS resolution is deferred to the asynchronous connection probe, not the UI monitor.
            if(IPAddress.TryParse(endpoint.Host,out var ip) && LanConnection.IsUnicast(ip))
            {
                var local=LanConnection.SelectSource(ip,endpoint.Port,"");
                return all.FirstOrDefault(a=>a.IpAddress==local.ToString()) ?? new BindIpCandidate { IpAddress=local.ToString(), AdapterName="系统路由",Reason="按服务器路由选择" };
            }
        }
        return all.FirstOrDefault();
    }
}
