using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.NetworkInformation;
using System.Net.Sockets;

namespace SplinterCellCNLauncher.Services;

/// <summary>IPv4 discovery targets; no provider names or assumed overlay prefixes.</summary>
public static class HostDiscoveryTargets
{
    public static IReadOnlyList<IPEndPoint> GetTargets(string? localIp, int port)
    {
        IPAddress? address = null;
        IPAddress? mask = null;
        if (IPAddress.TryParse(localIp, out address) && address.AddressFamily == AddressFamily.InterNetwork)
        {
            // Resolve only the selected local interface, never unrelated adapters.
            foreach (var adapter in NetworkInterface.GetAllNetworkInterfaces())
            {
                if (adapter.OperationalStatus != OperationalStatus.Up) continue;
                foreach (var unicast in adapter.GetIPProperties().UnicastAddresses)
                {
                    if (unicast.Address.Equals(address))
                    {
                        mask = unicast.IPv4Mask;
                        return GetTargets(address, mask, port);
                    }
                }
            }
        }
        return GetTargets(address, mask, port);
    }

    public static IReadOnlyList<IPEndPoint> GetTargets(IPAddress? address, IPAddress? mask, int port)
    {
        if (port < 1 || port > 65535) throw new ArgumentOutOfRangeException(nameof(port));
        var targets = new List<IPAddress> { IPAddress.Broadcast };
        if (address?.AddressFamily == AddressFamily.InterNetwork &&
            mask?.AddressFamily == AddressFamily.InterNetwork && !IPAddress.IsLoopback(address))
        {
            var a = address.GetAddressBytes();
            var m = mask.GetAddressBytes();
            uint bits = ((uint)m[0] << 24) | ((uint)m[1] << 16) | ((uint)m[2] << 8) | m[3];
            uint hostBits = ~bits;
            // Contiguous masks only; /0, /31 and /32 have no useful subnet broadcast.
            if (a[0] > 0 && a[0] < 224 && bits != 0 && hostBits > 1 &&
                (hostBits & (hostBits + 1)) == 0)
            {
                targets.Add(new IPAddress(a.Select((value, i) => (byte)(value | (m[i] ^ 255))).ToArray()));
            }
        }
        return targets.Distinct().Select(a => new IPEndPoint(a, port)).ToList();
    }
}
