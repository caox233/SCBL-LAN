using System.Globalization;
using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace SplinterCellCNLauncher.Services;

// Ordinary support export: never serializes a source line, unknown field, or file path.
internal static class DiagnosticSourceProjection
{
    private static Match TimedMatch(string input,string pattern,RegexOptions options=RegexOptions.None)=>Regex.Match(input,pattern,options,TimeSpan.FromMilliseconds(50));
    private static MatchCollection TimedMatches(string input,string pattern,RegexOptions options=RegexOptions.None)=>Regex.Matches(input,pattern,options,TimeSpan.FromMilliseconds(50));
    private static bool TimedIsMatch(string input,string pattern,RegexOptions options=RegexOptions.None)=>Regex.IsMatch(input,pattern,options,TimeSpan.FromMilliseconds(50));
    internal static int CountUnknownFields(string line)=>Field.Matches(line).Count(m=>!ExportFields.Contains(m.Groups[1].Value));
    internal sealed record Source(string Id, string Status, long? Bytes, int LinesRead, int Events, int Dropped, int UnknownFields = 0, long OmittedPrefixBytes = 0);
    internal sealed record Event(string Source, string? Utc, string Kind, string? Action, string? Result,
        string? Reason, uint? SessionId, uint? TypeId, int? ParticipantCount, uint? InvitationId, uint? TransportSession = null, string? FromState = null, string? ToState = null, uint? NativeCode = null, int? Maximum = null, int? PatchCount = null);
    internal sealed record Config(string? GameplayMode, double? Fov, bool? RememberPassword,
        string? BindIPv4, string? ServerIPv4, int? ServerPort, int? HostApiPort);
    internal sealed record Result(Source[] Sources, Event[] Events, Config Config);
    private static readonly HashSet<string> LoadoutActions = ["sidecar-capture", "sidecar-load", "sidecar-save", "clone", "serialize", "attachment-restore", "ensure", "unlock"];
    private static readonly HashSet<string> SavePhases = ["open", "list"];
    private static readonly HashSet<string> CapacityActions = ["create", "update", "leave", "add_participants", "remove_participants", "abandon", "join_recorded_unbounded", "split_restore"];
    private static readonly HashSet<string> Results = ["ok", "missing", "rejected", "skipped", "failed", "invalid-upgrades", "retail-assign-mismatch", "missing-source", "added", "invalid-manager"];
    private static readonly HashSet<string> Reasons = ["no-save-path", "no-manager", "io-busy", "sidecar-not-loaded", "rejected-sidecar", "incomplete-shadow-set", "size-exceeds-native-width", "native-contract", "hook", "native", "raw-owner-changed", "window-changed-restart-required", "existing-raw-owner", "register", "raw-read", "raw-size", "absolute-or-overflow", "raw-layout", "input-owner", "resident-lifetime", "initialize-hooks", "bridge-allocation", "bridge-executable", "enable-hooks"];
    private static readonly HashSet<string> PartyActions = ["PartySessionBound", "PrivateRoomPartyLeaderRestored", "PersonalLobbyPartyEnsured", "PartyFollowInviteSuppressedAlreadyJoined", "PartyGameInvitePayloadRejected", "BlacklistPartyAcceptedAbiBuilt", "PartyGameInviteAccepted", "FriendsPrivateBlobInviteAccepted", "QuickMatchSoloMemberLobbyRestoreQueued", "PrivateParticipantsDeferredUntilJoin", "HostLeaveAwaitingNativeMigration", "NativeHostMigrationExpired"];
    private static readonly Regex Field = new(@"(?:^|\s)([A-Za-z_]+)=([A-Za-z0-9_-]{1,48})(?=\s|$)", RegexOptions.Compiled,TimeSpan.FromMilliseconds(50));
    private static readonly HashSet<string> ExportFields = ["unix_ms", "enabled", "maximum", "patches", "code", "event", "phase", "result", "reason", "session_id", "target_session_id", "game_session_id", "lobby_session_id", "type_id", "session_type", "participant_count", "member_count", "members", "invitation_id", "session_not_found", "diagnostic_error", "initialized", "applied", "active"];
    private static readonly Regex Stamp = new(@"\b\d{4}-\d{2}-\d{2}[T ][0-9:.]{8,20}(?:Z|[+-][0-9:]{5})?\b", RegexOptions.Compiled,TimeSpan.FromMilliseconds(50));

    internal static Result Collect(string dataRoot, string? hostBind)
    {
        var sources = new List<Source>(); var events = new List<Event>();
        var config = new Config(null, null, null, null, null, null, null);
        string root = Path.GetFullPath(dataRoot);
        ReadLog("launcher", Path.Combine(root, "launcher.log"));
        string settings = Path.Combine(root, "launcher_settings.json");
        ReadConfig("launcher-settings", settings, true);
        string runs = Path.Combine(root, "diagnostics-v2");
        if (Directory.Exists(runs) && DiagnosticSafety.Plain(runs))
        {
            foreach (string dir in Directory.EnumerateDirectories(runs)
                .Where(p => Guid.TryParseExact(Path.GetFileName(p), "N", out _))
                .OrderByDescending(Directory.GetLastWriteTimeUtc).Take(4))
            {
                string id = Path.GetFileName(dir);
                ReadLog("native:" + id + ":main", Path.Combine(dir, "native", "logs", "game", "bl-tracing.log"));
                ReadLog("native:" + id + ":party", Path.Combine(dir, "native", "logs", "game", "hooks-party-trace.log"));
            }
        }
        else sources.Add(new("native-runs", Directory.Exists(runs) ? "ReparsePointSkipped" : "Missing", null, 0, 0, 0));
        if (IPAddress.TryParse(hostBind, out var ip) && ip.AddressFamily == AddressFamily.InterNetwork)
        {
            string host = Path.Combine(root, "server", ip.ToString().Replace('.', '_'));
            foreach (string name in new[] { "server.log.json", "server.stdout.txt", "server.stderr.txt", "init.stdout.txt", "init.stderr.txt" })
                ReadLog("host:" + name, Path.Combine(host, name));
            ReadConfig("host-service", Path.Combine(host, "service.toml"), false);
            ReadConfig("host-balancing", Path.Combine(host, "data", "mp_balancing.ini"), false);
        }
        else sources.Add(new("host", "NotSelected", null, 0, 0, 0));
        return new(sources.ToArray(), events.OrderBy(e => e.Utc).TakeLast(1024).ToArray(), config);

        void ReadConfig(string id, string path, bool settingsFile)
        {
            if (!Safe(path, 256 * 1024, out long bytes, out string status)) { sources.Add(new(id, status, bytes < 0 ? null : bytes, 0, 0, 0)); return; }
            try
            {
                if (settingsFile)
                {
                    using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                    using var document = JsonDocument.Parse(stream);
                    var item = document.RootElement;
                    if (item.ValueKind != JsonValueKind.Object) throw new JsonException();
                    string? mode = item.TryGetProperty("GameplayMode", out var m) && m.ValueKind == JsonValueKind.String ? GameplayRuntime.Normalize(m.GetString()) : null;
                    double? fov = item.TryGetProperty("FovScale", out var f) && f.ValueKind == JsonValueKind.Number && f.TryGetDouble(out var v) ? GameplayRuntime.NormalizeFov(v) : null;
                    bool? remember = item.TryGetProperty("RememberPassword", out var r) && r.ValueKind is JsonValueKind.True or JsonValueKind.False ? r.GetBoolean() : null;
                    string? bind = Address(item, "BindOverride", out _);
                    string? server = Address(item, "ServerAddress", out int? port);
                    config = config with { GameplayMode = mode, Fov = fov, RememberPassword = remember, BindIPv4 = bind, ServerIPv4 = server, ServerPort = port };
                    int unknownSettings = item.EnumerateObject().Count(p => p.Name is not ("GameplayMode" or "FovScale" or "RememberPassword" or "BindOverride" or "ServerAddress"));
                    sources.Add(new(id, "Projected", bytes, 0, 0, 0, unknownSettings));
                    return;
                }
                else if (id == "host-service")
                {
                    // Parse only the exact API address assignment, never TOML sections or strings.
                    foreach (string line in File.ReadLines(path).Take(4096))
                    {
                        var match = TimedMatch(line, "^api_server\\s*=\\s*\"([0-9.]+):([0-9]{1,5})\"\\s*$", RegexOptions.CultureInvariant);
                        if (match.Success && IPAddress.TryParse(match.Groups[1].Value, out var addr) && addr.AddressFamily == AddressFamily.InterNetwork && int.TryParse(match.Groups[2].Value, out int port) && port is > 0 and <= 65535)
                            config = config with { HostApiPort = port };
                    }
                }
                // Balancing file is deliberately presence-only until each tuning key has a reviewed parser.
                sources.Add(new(id, id == "host-balancing" ? "PresenceOnly" : "Projected", bytes, 0, 0, 0));
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException) { sources.Add(new(id, "UnsupportedSchema", bytes, 0, 0, 0)); }
        }
        void ReadLog(string id, string path)
        {
            if (!Safe(path, long.MaxValue, out long bytes, out string status)) { sources.Add(new(id, status, bytes < 0 ? null : bytes, 0, 0, 0)); return; }
            int lines = 0, count = 0, dropped = 0, unknown = 0;
            var selected = new Queue<Event>();
            try
            {
                using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                long start = Math.Max(0, stream.Length - 2L * 1024 * 1024);
                stream.Position = start;
                byte[] data = new byte[(int)Math.Min(2L * 1024 * 1024, stream.Length - start)];
                int filled = 0;
                while (filled < data.Length) { int read = stream.Read(data, filled, data.Length - filled); if (read == 0) break; filled += read; }
                using var reader = new StreamReader(new MemoryStream(data, 0, filled, false));
                if (start != 0) reader.ReadLine(); // Discard the first incomplete tail line.
                var tail = new Queue<string>();
                while (reader.ReadLine() is { } line)
                {
                    lines++;
                    if (tail.Count == 4096) { tail.Dequeue(); dropped++; }
                    tail.Enqueue(line);
                }
                foreach (string line in tail)
                {
                    if (line.Length > 8192) { dropped++; continue; }
                    unknown += Field.Matches(line).Count(m => !ExportFields.Contains(m.Groups[1].Value));
                    var eventValue = Project(id, line);
                    if (eventValue == null) { dropped++; continue; }
                    if (selected.Count == 256) { selected.Dequeue(); dropped++; }
                    selected.Enqueue(eventValue);
                }
                count = selected.Count;
                events.AddRange(selected);
                sources.Add(new(id, count == 0 && lines > 0 ? "NoRecognizedEvents" : start > 0 ? "TailProjected" : "Projected", bytes, lines, count, dropped, unknown, start));
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException) { sources.Add(new(id, "Unavailable", bytes, lines, count, dropped, unknown)); }
        }
    }
    private static string? Address(JsonElement item, string name, out int? port)
    {
        port = null;
        if (!item.TryGetProperty(name, out var v) || v.ValueKind != JsonValueKind.String) return null;
        string? value = v.GetString();
        if (value == null) return null;
        string[] pieces = value.Split(':');
        if (pieces.Length is < 1 or > 2 || !IPAddress.TryParse(pieces[0], out var ip) || ip.AddressFamily != AddressFamily.InterNetwork) return null;
        if (pieces.Length == 2 && int.TryParse(pieces[1], out int p) && p is > 0 and <= 65535) port = p;
        return ip.ToString();
    }
    private static bool Safe(string path, long cap, out long bytes, out string status)
    {
        bytes = -1; status = "Missing";
        try
        {
            if (!DiagnosticSafety.Plain(path)) { status = "ReparsePointSkipped"; return false; }
            if (!File.Exists(path)) return false;
            bytes = new FileInfo(path).Length;
            if (bytes > cap) { status = "TooLarge"; return false; }
            status = "Projected"; return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException) { status = "Unavailable"; return false; }
    }
    // Native Party traces use unix_ms instead of an ISO prefix. Keep this fallback
    // strict and bounded; do not export raw trace text or guess a missing time.
    private static string? UnixTimestamp(string line)
    {
        string? token = null;
        foreach (var field in line.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries))
        {
            if (!field.StartsWith("unix_ms=", StringComparison.Ordinal)) continue;
            if (token != null) return null;
            token = field[8..];
        }
        if (token == null || token.Length is < 1 or > 15 || token.Any(c => c is < '0' or > '9')
            || !long.TryParse(token, NumberStyles.None, CultureInfo.InvariantCulture, out long milliseconds)
            || milliseconds > 253402300799999L) return null;
        return DateTimeOffset.FromUnixTimeMilliseconds(milliseconds).ToString("O", CultureInfo.InvariantCulture);
    }
    // Existing native producer facts only. Export only allowlisted state labels; never personal names, IDs, pointers, paths or result text.
    private static readonly HashSet<string> TransitionStates = ["StateError", "StateHostSession", "StateIdle", "StateInMatch", "StateInSession", "StateIntermission", "StateJoin", "StateJoinMatch", "StateJoinSession", "StateJoinWaitForSettings", "StateJoiningOnlineSession", "StateLeave", "StateLeaveLocalMatch", "StateLeaveNetworkedMatch", "StateLeaveSession", "StateLeaveWithoutParty", "StateLeavingNetSession", "StateLeavingOnlineSession", "StateLoadingLobby", "StateLoadingMatch", "StateLobbyTransitionSync", "StateMatch", "StateMatchTransitionSync", "StateMatchmaking", "StateMigrateHost", "StateMigrating", "StatePartyRestore", "StateRestoreParty", "StateSessionShutdown", "StateShutdown"];
    private static readonly HashSet<string> CapacityFailures = ["host-executable-path-unavailable", "lobby-settings-entry-unavailable", "party-capacity-patches-unavailable", "invalid-patch-shape", "patch-signature-mismatch", "lobby-settings-hook-already-active", "pending-patch-restoration", "address-resolution-failed", "lobby-settings-hook-failed", "patch-state-poisoned", "patch-signature-changed", "patch-write-failed"];
    private static Event? ProjectNativeTransition(string source, string line, string? utc)
    {
        if (line.Length > 8192) return null;
        string? Single(string name)
        {
            var values = TimedMatches(line, @"(?:^|\s)" + Regex.Escape(name) + @"=([^\s]+)(?=\s|$)");
            return values.Count == 1 ? values[0].Groups[1].Value : null;
        }
        string? State(string value) => TransitionStates.Contains(value) ? value : null;
        if (TimedIsMatch(line, @"\ASCBL_MOD_PLAYER_CAPACITY(?:\s|$)"))
        {
            if (TimedIsMatch(line, @"\ASCBL_MOD_PLAYER_CAPACITY enabled=true maximum=12 patches=[0-9]{1,3}\z")
                && Single("enabled") == "true" && Single("maximum") == "12"
                && int.TryParse(Single("patches"), NumberStyles.None, CultureInfo.InvariantCulture, out int patches)
                && patches is >= 1 and <= 128)
                return new(source, utc, "native-capacity", "installed", "enabled", null, null, null, null, null, Maximum: 12, PatchCount: patches);
            if (TimedIsMatch(line, @"\ASCBL_MOD_PLAYER_CAPACITY enabled=false reason=[a-z-]+(?: .*)?\z")
                && Single("enabled") == "false" && Single("reason") is { } reason && CapacityFailures.Contains(reason))
                return new(source, utc, "native-capacity", "install-failed", "disabled", reason, null, null, null, null);
            if (TimedIsMatch(line, @"\ASCBL_MOD_PLAYER_CAPACITY applied owner=0x[0-9A-Fa-f]{1,8} settings=0x[0-9A-Fa-f]{1,8} previous=[0-9]{1,10} maximum=12\z") && Single("maximum") == "12")
                return new(source, utc, "native-capacity", "applied", null, null, null, null, null, null, Maximum: 12);
            return null;
        }
        var result = TimedMatches(line, @"\ANetResult(Core|Session|RdvSession|Lobby): code=([0-9A-Fa-f]{1,8}) text=");
        if (result.Count == 1 && uint.TryParse(result[0].Groups[2].Value, NumberStyles.HexNumber, CultureInfo.InvariantCulture, out uint code))
            return new(source, utc, "native-net-result", result[0].Groups[1].Value, null, null, null, null, null, null, NativeCode: code);
        var leave = TimedMatch(line, @"\ALeaving state (State[A-Za-z]+)\(inst=0x[0-9A-Fa-f]{1,8}, vtable=0x[0-9A-Fa-f]{1,8}, id=[0-9A-Fa-f]{1,8}\)\z");
        if (leave.Success && State(leave.Groups[1].Value) is { } from)
            return new(source, utc, "native-state", "leave", null, null, null, null, null, null, FromState: from);
        var next = TimedMatch(line, @"\ANext state: (State[A-Za-z]+) StateMachine\(inst=0x[0-9A-Fa-f]{1,8}, vtable=0x[0-9A-Fa-f]{1,8}, name=[^\r\n]{1,256}\) current=(State[A-Za-z]+)\(inst=0x[0-9A-Fa-f]{1,8}, vtable=0x[0-9A-Fa-f]{1,8}, id=[0-9A-Fa-f]{1,8}\) last=[^\r\n]{1,256}\z");
        if (next.Success && State(next.Groups[1].Value) is { } to && State(next.Groups[2].Value) is { } current)
            return new(source, utc, "native-state", "next", null, null, null, null, null, null, FromState: current, ToState: to);
        var change = TimedMatch(line, @"\AGoal [^\r\n]{1,512} with [0-9]{1,10}, state=(State[A-Za-z]+)\(id=[0-9A-Fa-f]{1,8}\), next_state_id=(State[A-Za-z]+)\(id=[0-9A-Fa-f]{1,8}\)\z");
        if (change.Success
            && State(change.Groups[1].Value) is { } prior && State(change.Groups[2].Value) is { } following)
            return new(source, utc, "native-state", "change", null, null, null, null, null, null, FromState: prior, ToState: following);
        return null;
    }
    internal static Event? Project(string source, string line)
    {
        // Never emit a matched unknown token. Each exported textual value is from a code allowlist.
        if (line.TrimStart().StartsWith('{'))
        {
            try
            {
                using var json = JsonDocument.Parse(line);
                var root = json.RootElement;
                if (root.ValueKind != JsonValueKind.Object || !root.TryGetProperty("msg", out var message) || message.ValueKind != JsonValueKind.String) return null;
                string msg = message.GetString() ?? "";
                Event? inner = Project(source, msg) ?? ProjectServerMessage(source, root, msg);
                if (inner == null) return null;
                if (root.TryGetProperty("ts", out var time) && time.ValueKind == JsonValueKind.String && DateTimeOffset.TryParse(time.GetString(), out var jsonTime))
                    return inner with { Utc = jsonTime.ToUniversalTime().ToString("O") };
                return inner;
            }
            catch (JsonException) { return null; }
        }
        string? stamp = null;
        var ts = Stamp.Match(line);
        if (ts.Success && DateTimeOffset.TryParse(ts.Value, out var parsed)) stamp = parsed.ToUniversalTime().ToString("O");
        stamp ??= UnixTimestamp(line);
        var native = TimedMatch(line, @"(?:^|\s)(?:SCBL_MOD_PLAYER_CAPACITY(?:\s|$)|NetResult(?:Core|Session|RdvSession|Lobby):|Leaving state |Next state: |Goal )");
        if (native.Success)
            return line.Length > 8192 ? null : ProjectNativeTransition(source, line[native.Index..].TrimStart(), stamp);
        var fields = Field.Matches(line).GroupBy(m => m.Groups[1].Value, StringComparer.Ordinal)
            .ToDictionary(g => g.Key, g => g.Last().Groups[2].Value, StringComparer.Ordinal);
        string? Get(string key) => fields.GetValueOrDefault(key);
        string? Allowed(string? value, HashSet<string> set) => value != null && set.Contains(value) ? value : null;
        uint? Unsigned(string key) => uint.TryParse(Get(key), out uint v) ? v : null;
        uint? FirstUnsigned(params string[] keys) => keys.Select(Unsigned).FirstOrDefault(v => v.HasValue);
        int? Count(string key) => int.TryParse(Get(key), out int v) && v is >= 0 and <= 64 ? v : null;
        if (line.Contains("SCBL_EXTENDED_LOADOUTS", StringComparison.Ordinal))
        {
            string? action = Allowed(Get("event"), LoadoutActions);
            if (action == null) return null;
            return new(source, stamp, "loadout", action, Allowed(Get("result"), Results), Allowed(Get("reason"), Reasons), null, null, null, null);
        }
        if (line.Contains("SCBL_SAVE_COMPAT", StringComparison.Ordinal))
        {
            string? phase = Allowed(Get("phase"), SavePhases);
            if (phase == null) return null;
            return new(source, stamp, "save-compat", phase, null, Allowed(Get("reason"), Reasons), null, null, null, null);
        }
        if (line.Contains("SCBL_SESSION_CAPACITY", StringComparison.Ordinal))
        {
            string? action = Allowed(Get("event"), CapacityActions);
            if (action == null) return null;
            return new(source, stamp, "session-capacity", action, Get("session_not_found") == "true" ? "not-found" : Get("diagnostic_error") != null ? "diagnostic-error" : null,
                null, Unsigned("session_id"), Unsigned("type_id"), Count("participant_count"), null);
        }
        if (line.Contains("SCBL_CORE_MERC_MOUSE", StringComparison.Ordinal) || line.Contains("SCBL_CORE_RAW_MOUSE", StringComparison.Ordinal))
        {
            string kind = line.Contains("MERC_MOUSE", StringComparison.Ordinal) ? "merc-mouse" : "raw-mouse";
            string? action = Get("initialized") == "true" ? "initialized" : Get("initialized") == "false" ? "init-failed" : Get("applied") == "true" ? "applied" : Get("active") == "false" ? "fallback" : null;
            return action == null ? null : new(source, stamp, kind, action, null, Allowed(Get("reason"), Reasons), null, null, null, null);
        }
        string? party = PartyActions.FirstOrDefault(p => line.Contains(p, StringComparison.Ordinal));
        return party == null ? null : new(source, stamp, "party", party, null, null,
            FirstUnsigned("session_id", "target_session_id", "game_session_id", "lobby_session_id"), FirstUnsigned("type_id", "session_type"), Count("member_count") ?? Count("members"), Unsigned("invitation_id"));
    }
    private static Event? ProjectServerMessage(string source, JsonElement root, string msg)
    {
        uint? Number(string name) => root.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetUInt32(out uint n) ? n : null;
        int? Bounded(string name) => root.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out int n) && n is >= 0 and <= 100000 ? n : null;
        Event Server(string kind, string action, string? reason = null) => new(source, null, kind, action, null, reason, Number("session_id"), Number("type_id"), null, null);
        return msg switch
        {
            "Client disconnected" => Server("transport", "disconnected") with { TransportSession = Number("session") },
            "New client connected" => Server("transport", "connected") with { TransportSession = Number("session") },
            "Rejected join for missing or destroyed session" => Server("session-join", "rejected", "missing-or-destroyed"),
            "Session delete rejected for non-owner or missing session" => Server("session-delete", "rejected", "non-owner-or-missing"),
            "Ignoring session without a valid host participant" => Server("session-search", "ignored", "missing-host-participant"),
            "Ignoring session whose host has no registered endpoint" => Server("session-search", "ignored", "missing-host-endpoint"),
            "Ignoring session with invalid host endpoints" => Server("session-search", "ignored", "invalid-host-endpoints"),
            "Ignoring session with invalid attributes" => Server("session-search", "ignored", "invalid-attributes"),
            "LobbyRestoreDeferredRemovalCompleted" => Server("party", "lobby-restore-cleanup") with { ParticipantCount = Bounded("resolved_members") },
            "NativeHostMigrationFallbackCompleted" => Server("party", "host-migration-fallback") with { ParticipantCount = Bounded("resolved_sessions") },
            "QuickMatchMemberLeaveFallbackCompleted" => Server("party", "member-leave-fallback") with { ParticipantCount = Bounded("resolved_members") },
            _ when msg.StartsWith("Client records unbounded session join:", StringComparison.Ordinal) => Server("session-join", "requested"),
            _ when msg.StartsWith("Client leaves session:", StringComparison.Ordinal) => Server("session-leave", "requested"),
            _ => null
        };
    }
}
