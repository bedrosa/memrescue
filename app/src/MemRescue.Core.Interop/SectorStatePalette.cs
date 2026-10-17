namespace MemRescue.Core.Interop;

/// <summary>Colores (0xRRGGBB) y marca de texto con que el visor presenta un estado de sector.</summary>
/// <param name="Background">Fondo del bloque del sector.</param>
/// <param name="Foreground">Texto sobre ese fondo (contraste ≥ 7:1, WCAG AAA).</param>
/// <param name="Tag">Marca textual breve: el estado nunca se comunica solo con color.</param>
public readonly record struct SectorStateStyle(uint Background, uint Foreground, string Tag);

/// <summary>
/// Paleta del visor por estado de sector. Los no fiables (ilegible, saltado) usan tonos cálidos
/// distintos entre sí y de los fiables; el texto siempre conserva marcas («!! SECTOR NO FIABLE»,
/// la etiqueta del estado) para no depender del color. Con temas de alto contraste el visor ignora
/// esta paleta y usa los colores del sistema.
/// </summary>
public static class SectorStatePalette
{
    public static SectorStateStyle For(SectorState s) => s switch
    {
        SectorState.Read => new(0xFFFFFF, 0x1B1B1B, "LEÍDO"),
        SectorState.Recovered => new(0xE3F2FD, 0x0D2C54, "RECUPERADO"),
        SectorState.Unreadable => new(0xFDE0E0, 0x7A0000, "ILEGIBLE · RELLENO"),
        SectorState.Skipped => new(0xFFF0C2, 0x5C3B00, "SALTADO · RELLENO"),
        SectorState.Sparse => new(0xECEFF1, 0x263238, "DISPERSO · CEROS"),
        _ => new(0xFFFFFF, 0x000000, "DESCONOCIDO"),
    };

    public static IReadOnlyList<SectorState> AllStates { get; } =
        [SectorState.Read, SectorState.Recovered, SectorState.Unreadable, SectorState.Skipped, SectorState.Sparse];

    /// <summary>Razón de contraste WCAG 2.x entre dos colores 0xRRGGBB (1 a 21).</summary>
    public static double ContrastRatio(uint a, uint b)
    {
        var (la, lb) = (Luminance(a), Luminance(b));
        return (Math.Max(la, lb) + 0.05) / (Math.Min(la, lb) + 0.05);
    }

    private static double Luminance(uint rgb)
    {
        static double Channel(uint c)
        {
            var v = c / 255.0;
            return v <= 0.03928 ? v / 12.92 : Math.Pow((v + 0.055) / 1.055, 2.4);
        }
        return 0.2126 * Channel((rgb >> 16) & 0xFF) + 0.7152 * Channel((rgb >> 8) & 0xFF) + 0.0722 * Channel(rgb & 0xFF);
    }
}
