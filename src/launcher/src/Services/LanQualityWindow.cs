namespace SplinterCellCNLauncher.Services;

public sealed record LanQualitySnapshot(int Samples, int Received, double? RttMs, double? JitterMs, double LossPercent, string State);

// A bounded UDP application-probe window, NOT a claim about game UDP ports or room joinability.
public sealed class LanQualityWindow
{
    private readonly Queue<double?> samples = new();
    public void Clear() => samples.Clear();
    public void Add(double? elapsedMs)
    {
        samples.Enqueue(elapsedMs is >= 0 and <= 60000 && double.IsFinite(elapsedMs.Value) ? elapsedMs : null);
        while (samples.Count > 30) samples.Dequeue();
    }
    public LanQualitySnapshot Snapshot()
    {
        var values = samples.Where(x => x.HasValue).Select(x => x!.Value).ToArray();
        double loss = samples.Count == 0 ? 0 : 100.0 * (samples.Count - values.Length) / samples.Count;
        double? rtt = values.Length == 0 ? null : values.Average();
        double? jitter = values.Length < 2 ? null : values.Zip(values.Skip(1), (a,b) => Math.Abs(a-b)).Average();
        string state = samples.Count < 3 ? "sampling" : values.Length == 0 ? "no-reply" :
            loss >= 20 || rtt > 150 || jitter > 50 ? "unstable" : loss > 0 || rtt > 80 || jitter > 20 ? "fair" : "good";
        return new(samples.Count, values.Length, rtt, jitter, loss, state);
    }
}
