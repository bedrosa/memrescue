using System.Globalization;
using System.Text;

namespace MemRescue.Core.Interop;

/// <summary>Formato de volcados hexadecimales y análisis de direcciones introducidas por el usuario.</summary>
public static class HexDump
{
    public const int BytesPerLine = 16;

    /// <summary>
    /// Volcado de <paramref name="data"/> con desplazamientos absolutos (<paramref name="baseOffset"/>).
    /// Formato por línea: <c>OOOOOOOOOOOO  HH HH … HH  HH … HH  ASCII</c>.
    /// </summary>
    public static string Format(ReadOnlySpan<byte> data, ulong baseOffset)
    {
        var sb = new StringBuilder((data.Length / BytesPerLine + 1) * 80);
        for (var i = 0; i < data.Length; i += BytesPerLine)
        {
            var line = data.Slice(i, Math.Min(BytesPerLine, data.Length - i));
            sb.Append((baseOffset + (ulong)i).ToString("X12", CultureInfo.InvariantCulture)).Append("  ");
            for (var j = 0; j < BytesPerLine; j++)
            {
                if (j == 8)
                {
                    sb.Append(' ');
                }
                sb.Append(j < line.Length ? line[j].ToString("X2", CultureInfo.InvariantCulture) : "  ").Append(' ');
            }
            sb.Append(' ');
            foreach (var b in line)
            {
                sb.Append(b is >= 0x20 and < 0x7F ? (char)b : '.');
            }
            sb.Append('\n');
        }
        return sb.ToString();
    }

    /// <summary>Analiza un número decimal o hexadecimal (<c>0x…</c> o sufijo <c>h</c>); admite separadores.</summary>
    public static bool TryParseNumber(string? text, out ulong value)
    {
        value = 0;
        if (string.IsNullOrWhiteSpace(text))
        {
            return false;
        }
        var t = text.Trim().Replace("_", "", StringComparison.Ordinal).Replace(" ", "", StringComparison.Ordinal)
            .Replace(".", "", StringComparison.Ordinal);
        if (t.StartsWith("0x", StringComparison.OrdinalIgnoreCase))
        {
            return ulong.TryParse(t.AsSpan(2), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out value);
        }
        if (t.EndsWith('h') || t.EndsWith('H'))
        {
            return ulong.TryParse(t.AsSpan(0, t.Length - 1), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out value);
        }
        return ulong.TryParse(t, NumberStyles.None, CultureInfo.InvariantCulture, out value);
    }
}
