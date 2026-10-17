using System.Globalization;

namespace MemRescue.Core.Interop;

/// <summary>
/// Análisis de la lista de códigos de error del sistema que la verificación no reintenta
/// (política de omisión de errores). Formato: números separados por comas, punto y coma o
/// espacios; cada uno en decimal o hexadecimal con prefijo <c>0x</c> (p. ej. <c>23, 1117</c>).
/// </summary>
public static class ErrorCodeList
{
    /// <summary>Máximo de códigos admitidos por el motor.</summary>
    public const int MaxCodes = 256;

    private static readonly char[] Separators = [',', ';', ' ', '\t', '\r', '\n'];

    /// <summary>
    /// Analiza la lista. Devuelve <c>false</c> y un mensaje en español si algún elemento no es un
    /// código positivo o hay más de <see cref="MaxCodes"/>. Texto vacío = lista vacía. El resultado
    /// queda ordenado y sin duplicados.
    /// </summary>
    public static bool TryParse(string? text, out int[] codes, out string? error)
    {
        codes = [];
        error = null;
        var set = new SortedSet<int>();
        foreach (var part in (text ?? "").Split(Separators, StringSplitOptions.RemoveEmptyEntries))
        {
            var ok = part.StartsWith("0x", StringComparison.OrdinalIgnoreCase)
                ? int.TryParse(part.AsSpan(2), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out var v)
                : int.TryParse(part, NumberStyles.None, CultureInfo.InvariantCulture, out v);
            if (!ok || v <= 0)
            {
                error = $"Código de error no válido: «{part}» (debe ser un número positivo).";
                return false;
            }
            set.Add(v);
        }
        if (set.Count > MaxCodes)
        {
            error = $"Demasiados códigos de error ({set.Count}); el máximo es {MaxCodes}.";
            return false;
        }
        codes = [.. set];
        return true;
    }

    /// <summary>Texto canónico de una lista (códigos separados por «, »).</summary>
    public static string Format(IEnumerable<int> codes) =>
        string.Join(", ", codes.Select(c => c.ToString(CultureInfo.InvariantCulture)));
}
