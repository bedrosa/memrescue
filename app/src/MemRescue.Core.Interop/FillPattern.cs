using System.Globalization;
using System.Text;

namespace MemRescue.Core.Interop;

/// <summary>
/// Patrón de relleno de 4 bytes que el motor escribe, repetido, en los sectores que no contienen datos
/// del origen (ilegibles o saltados). El relleno NUNCA son datos: la única fuente de verdad es el
/// <see cref="SectorState"/> de cada sector.
/// </summary>
public readonly record struct FillPattern(byte B0, byte B1, byte B2, byte B3)
{
    /// <summary>Relleno por defecto de los sectores ilegibles: <c>BAD!</c>.</summary>
    public static FillPattern DefaultBad { get; } = new((byte)'B', (byte)'A', (byte)'D', (byte)'!');

    /// <summary>Relleno por defecto de los sectores saltados: <c>SKIP</c>.</summary>
    public static FillPattern DefaultSkip { get; } = new((byte)'S', (byte)'K', (byte)'I', (byte)'P');

    public byte[] ToArray() => [B0, B1, B2, B3];

    /// <summary><c>true</c> si los cuatro bytes son iguales (incluido <c>00 00 00 00</c>).</summary>
    public bool IsUniform => B0 == B1 && B1 == B2 && B2 == B3;

    public bool IsZero => IsUniform && B0 == 0;

    /// <summary>Los cuatro bytes en hexadecimal sin separadores (<c>42414421</c>): formato persistente.</summary>
    public string ToHex() => Convert.ToHexString(ToArray());

    /// <summary>Texto para el usuario: los 4 caracteres si son ASCII imprimible; si no, 8 dígitos hexadecimales.</summary>
    public string ToDisplayText() =>
        ToArray().All(b => b is >= 0x20 and < 0x7F) ? Encoding.ASCII.GetString(ToArray()) : ToHex();

    /// <summary>Descripción completa: <c>«BAD!» (42 41 44 21)</c> o <c>DE AD BE EF</c>.</summary>
    public string Describe()
    {
        var hex = string.Join(' ', ToArray().Select(b => b.ToString("X2", CultureInfo.InvariantCulture)));
        return ToArray().All(b => b is >= 0x20 and < 0x7F) ? $"«{Encoding.ASCII.GetString(ToArray())}» ({hex})" : hex;
    }

    public override string ToString() => ToDisplayText();

    /// <summary>
    /// Analiza un patrón escrito por el usuario (misma regla que el motor y la CLI): exactamente 4
    /// caracteres ASCII imprimibles (<c>BAD!</c>) o exactamente 8 dígitos hexadecimales con prefijo
    /// <c>0x</c> y espacios opcionales (<c>DEADBEEF</c>, <c>0xDE AD BE EF</c>). No aplica
    /// <see cref="Validate"/>.
    /// </summary>
    public static bool TryParse(string? text, out FillPattern pattern, out string? error)
    {
        pattern = default;
        error = null;
        text ??= "";
        if (text.Length == 4 && text.All(c => c is >= ' ' and < '\x7F'))
        {
            pattern = new((byte)text[0], (byte)text[1], (byte)text[2], (byte)text[3]);
            return true;
        }
        var t = text.Trim();
        if (t.StartsWith("0x", StringComparison.OrdinalIgnoreCase))
        {
            t = t[2..];
        }
        var hex = new string([.. t.Where(c => !char.IsWhiteSpace(c))]);
        if (hex.Length == 8 && hex.All(char.IsAsciiHexDigit))
        {
            var b = Convert.FromHexString(hex);
            pattern = new(b[0], b[1], b[2], b[3]);
            return true;
        }
        error = $"Patrón no válido: «{text}». Escriba 4 caracteres ASCII (p. ej. BAD!) u 8 dígitos hexadecimales (p. ej. DEADBEEF).";
        return false;
    }

    /// <summary>
    /// Regla de admisión de un par de patrones (la misma que aplica el motor en
    /// <c>mr_source_read_mapped_ex</c>): ninguno puede tener sus cuatro bytes iguales
    /// (<c>00 00 00 00</c> se confundiría con sectores dispersos o datos a cero; <c>FF FF FF FF</c> y
    /// similares, con rellenos habituales en datos reales) y ilegibles y saltados deben ser distintos.
    /// Devuelve <c>null</c> si el par es válido o el motivo en español.
    /// </summary>
    public static string? Validate(FillPattern bad, FillPattern skip)
    {
        foreach (var (p, name) in new[] { (bad, "ilegibles"), (skip, "saltados") })
        {
            if (p.IsZero)
            {
                return $"El patrón de los sectores {name} no puede ser 00 00 00 00: se confundiría con sectores dispersos o datos a cero.";
            }
            if (p.IsUniform)
            {
                return $"El patrón de los sectores {name} no puede tener los cuatro bytes iguales: se confundiría con rellenos habituales en datos reales.";
            }
        }
        return bad == skip ? "Los patrones de ilegibles y saltados deben ser distintos para poder distinguirlos." : null;
    }
}
