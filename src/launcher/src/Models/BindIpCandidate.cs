using System;
namespace SplinterCellCNLauncher.Models;

public sealed class BindIpCandidate
{
    public string AdapterName { get; init; } = "";
    public string Description { get; init; } = "";
    public string IpAddress { get; init; } = "";
    public int Score { get; init; }
    public string Reason { get; init; } = "";
}
