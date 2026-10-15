using System.Text;

namespace SplinterCellCNLauncher.Services;

internal static class PasswordPolicy
{
    // Capacity/encoding boundaries only, not password strength or character-class rules.
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);
    internal static bool IsValidInput(string text)
    {
        if (text.Contains('\0')) return false; // Native game credential strings cannot carry NUL.
        try { return StrictUtf8.GetByteCount(text) <= 4096; }
        catch (EncoderFallbackException) { return false; }
    }
    internal static bool IsValidPassword(string text) => text.Length != 0 && IsValidInput(text);

    // Retail GetPassword receives a 64-byte buffer, including the trailing NUL.
    // This is a game-launch constraint, not a change to the account API policy.
    internal const int NativePasswordBufferBytes = 64;
    internal static bool IsValidForNativeGame(string text)
        => IsValidPassword(text) && StrictUtf8.GetByteCount(text) < NativePasswordBufferBytes;

}
