using System.Runtime.InteropServices;
using System.Security.Cryptography;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>
/// Patrones de relleno configurables (IO-08) a través de <c>mr_source_read_mapped_ex</c>, preferencias
/// persistentes y paleta del visor. Las imágenes y el fichero de preferencias viven en un directorio
/// temporal propio: nunca se toca el perfil del usuario.
/// </summary>
public sealed class FillPatternTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-fill-").FullName;

    public void Dispose()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        Directory.Delete(_dir, recursive: true);
    }

    private static readonly FillPattern DeadBeef = new(0xDE, 0xAD, 0xBE, 0xEF);
    private static readonly FillPattern Sk1p = new((byte)'s', (byte)'k', (byte)'1', (byte)'p');

    private static bool Repeats(ReadOnlySpan<byte> sector, FillPattern p)
    {
        var a = p.ToArray();
        for (var i = 0; i < sector.Length; i++)
        {
            if (sector[i] != a[i % 4])
            {
                return false;
            }
        }
        return true;
    }

    private string DamagedVhd()
    {
        var p = Path.Combine(_dir, "t.vhd");
        File.WriteAllBytes(p, ContainerTests.DamagedDynamicVhd());
        return p;
    }

    [Fact]
    public void OptionsStructMatchesHeader()
    {
        Assert.Equal(32, Marshal.SizeOf<MrReadOptions>());
        Assert.Equal(20, (int)Marshal.OffsetOf<MrReadOptions>(nameof(MrReadOptions.BadFill)));
        Assert.Equal(24, (int)Marshal.OffsetOf<MrReadOptions>(nameof(MrReadOptions.SkipFill)));
    }

    [Theory]
    [InlineData("BAD!", "42414421")]
    [InlineData("a b ", "61206220")]
    [InlineData("DEADBEEF", "DEADBEEF")]
    [InlineData("0xde ad be ef", "DEADBEEF")]
    [InlineData(" 0X0102A0FF ", "0102A0FF")]
    public void PatternsAreParsedLikeTheEngine(string text, string hex)
    {
        Assert.True(FillPattern.TryParse(text, out var p, out var error), error);
        Assert.Equal(hex, p.ToHex());
        Assert.True(FillPattern.TryParse(p.ToHex(), out var again, out _));
        Assert.Equal(p, again);
    }

    [Theory]
    [InlineData("")]
    [InlineData("BAD")]
    [InlineData("BAD!!")]
    [InlineData("DEADBEEG")]
    [InlineData("0xDEADBE")]
    [InlineData("ñañ")]
    [InlineData("AB\tC")]
    public void InvalidPatternTextIsRejected(string text)
    {
        Assert.False(FillPattern.TryParse(text, out _, out var error));
        Assert.Contains("Patrón no válido", error);
    }

    [Fact]
    public void ValidationRuleMatchesTheEngine()
    {
        Assert.Null(FillPattern.Validate(FillPattern.DefaultBad, FillPattern.DefaultSkip));
        Assert.Null(FillPattern.Validate(DeadBeef, Sk1p));
        Assert.Contains("00 00 00 00", FillPattern.Validate(new(0, 0, 0, 0), Sk1p));
        Assert.Contains("saltados", FillPattern.Validate(DeadBeef, new(0, 0, 0, 0)));
        Assert.Contains("cuatro bytes iguales", FillPattern.Validate(new(0xFF, 0xFF, 0xFF, 0xFF), Sk1p));
        Assert.Contains("distintos", FillPattern.Validate(DeadBeef, DeadBeef));
        Assert.Equal("«BAD!» (42 41 44 21)", FillPattern.DefaultBad.Describe());
        Assert.Equal("DE AD BE EF", DeadBeef.Describe());
        Assert.Equal("DEADBEEF", DeadBeef.ToDisplayText());
        Assert.Equal("SKIP", FillPattern.DefaultSkip.ToDisplayText());
    }

    [Fact]
    public void ConfiguredPatternsReachUnreadableAndSkippedSectors()
    {
        var p = DamagedVhd();
        var before = SHA256.HashData(File.ReadAllBytes(p));
        using var src = Engine.OpenSource(p, tolerant: true);
        // Bloque 1 (LBA 8–15) descartado: ilegible. Con salto de 3, LBA 9–11 quedan saltados.
        var m = src.ReadSectorsMapped(6, 10, retries: 0, badFill: DeadBeef, skipFill: Sk1p, jumpSectors: 3);
        SectorState[] expected =
        [
            SectorState.Read, SectorState.Read, SectorState.Unreadable, SectorState.Skipped, SectorState.Skipped,
            SectorState.Skipped, SectorState.Unreadable, SectorState.Skipped, SectorState.Skipped, SectorState.Skipped,
        ];
        Assert.Equal(expected, m.States);
        for (var i = 0; i < m.States.Length; i++)
        {
            var sector = m.Data.AsSpan(i * 512, 512);
            switch (m.States[i])
            {
                case SectorState.Unreadable:
                    Assert.True(Repeats(sector, DeadBeef), $"LBA {6 + i}");
                    break;
                case SectorState.Skipped:
                    Assert.True(Repeats(sector, Sk1p), $"LBA {6 + i}");
                    break;
                default:
                    Assert.All(sector.ToArray(), b => Assert.Equal(0x10, b));
                    break;
            }
        }
        // Sin salto, solo el relleno de ilegibles; los dispersos siguen siendo ceros fiables.
        var n = src.ReadSectorsMapped(14, 12, retries: 0, badFill: DeadBeef, skipFill: Sk1p);
        Assert.Equal(SectorState.Unreadable, n.States[0]);
        Assert.True(Repeats(n.Data.AsSpan(0, 512), DeadBeef));
        Assert.Equal(SectorState.Sparse, n.States[^1]);
        Assert.All(n.Data[^512..], b => Assert.Equal(0, b));
        // Sin patrones: el comportamiento clásico (BAD!) no cambia.
        var d = src.ReadSectorsMapped(8, 1);
        Assert.True(Repeats(d.Data, FillPattern.DefaultBad));
        Assert.Equal(before, SHA256.HashData(File.ReadAllBytes(p)));
    }

    [Fact]
    public void EngineRejectsPatternsThatCouldPassForData()
    {
        using var src = Engine.OpenSource(DamagedVhd(), tolerant: true);
        (FillPattern Bad, FillPattern Skip, string Message)[] cases =
        [
            (new(0, 0, 0, 0), Sk1p, "00 00 00 00"),
            (DeadBeef, new(0, 0, 0, 0), "saltados"),
            (new(0xFF, 0xFF, 0xFF, 0xFF), Sk1p, "cuatro bytes iguales"),
            (DeadBeef, DeadBeef, "distintos"),
        ];
        foreach (var (bad, skip, message) in cases)
        {
            var ex = Assert.Throws<EngineException>(() => src.ReadSectorsMapped(8, 1, badFill: bad, skipFill: skip));
            Assert.Equal(EngineStatus.InvalidArgument, ex.Status);
            Assert.Contains(message, ex.Message);
        }
    }

    [Fact]
    public async Task HashNeverIncludesFillWhateverThePattern()
    {
        using var src = Engine.OpenSource(DamagedVhd(), tolerant: true);
        _ = src.ReadSectorsMapped(0, 32, badFill: DeadBeef, skipFill: Sk1p);
        using var h = src.StartHash(HashAlgorithms.Sha256);
        Assert.Equal(EngineTaskState.Failed, await h.WaitAsync());
    }

    [Fact]
    public void SettingsRoundTripAndDefaults()
    {
        var store = new SettingsStore(Path.Combine(_dir, "prefs", SettingsStore.FileName));
        // Ausente: valores por defecto sin aviso.
        Assert.Equal(UserSettings.Defaults, store.Load(out var warning));
        Assert.Null(warning);
        store.Save(new UserSettings { BadFill = DeadBeef, SkipFill = Sk1p });
        Assert.Equal(new UserSettings { BadFill = DeadBeef, SkipFill = Sk1p }, store.Load(out warning));
        Assert.Null(warning);
        var json = File.ReadAllText(store.Path);
        Assert.Contains("\"bad_fill\": \"DEADBEEF\"", json);
        Assert.Contains("\"skip_fill\": \"736B3170\"", json);
        Assert.False(File.Exists(store.Path + ".tmp"));
        // Guardar valores no admitidos: se rechaza y el fichero no cambia.
        Assert.Throws<ArgumentException>(() => store.Save(new UserSettings { BadFill = new(0, 0, 0, 0) }));
        Assert.Equal(json, File.ReadAllText(store.Path));
    }

    [Theory]
    [InlineData("{ esto no es JSON")]
    [InlineData("null")]
    [InlineData("{\"version\":1,\"bad_fill\":\"00000000\",\"skip_fill\":\"534B4950\"}")]
    [InlineData("{\"version\":1,\"bad_fill\":\"534B4950\",\"skip_fill\":\"534B4950\"}")]
    [InlineData("{\"version\":1,\"bad_fill\":\"XYZ\"}")]
    [InlineData("{\"version\":1,\"bad_fill\":42}")]
    public void CorruptSettingsFallBackToDefaults(string content)
    {
        var path = Path.Combine(_dir, SettingsStore.FileName);
        File.WriteAllText(path, content);
        var store = new SettingsStore(path);
        Assert.Equal(UserSettings.Defaults, store.Load(out var warning));
        Assert.False(string.IsNullOrEmpty(warning));
        // El fichero dañado no se sobrescribe al leerlo.
        Assert.Equal(content, File.ReadAllText(path));
    }

    [Fact]
    public void OversizedSettingsFileIsIgnored()
    {
        var path = Path.Combine(_dir, SettingsStore.FileName);
        File.WriteAllText(path, new string(' ', SettingsStore.MaxFileBytes + 1));
        Assert.Equal(UserSettings.Defaults, new SettingsStore(path).Load(out var warning));
        Assert.Contains("demasiado grande", warning);
    }

    [Fact]
    public void SettingsLocationHonoursTheOverride()
    {
        var appData = Path.Combine(_dir, "AppData");
        Assert.Equal(Path.Combine(appData, "MemRescue", "settings.json"), SettingsStore.ResolvePath(null, appData));
        Assert.Equal(Path.Combine(appData, "MemRescue", "settings.json"), SettingsStore.ResolvePath("  ", appData));
        Assert.Equal(Path.Combine(_dir, "x", "settings.json"), SettingsStore.ResolvePath(Path.Combine(_dir, "x"), appData));
    }

    [Fact]
    public void PaletteIsDistinctAndReadable()
    {
        var styles = SectorStatePalette.AllStates.Select(SectorStatePalette.For).ToList();
        Assert.Equal(5, styles.Select(s => s.Background).Distinct().Count());
        Assert.Equal(5, styles.Select(s => s.Tag).Distinct().Count());
        foreach (var s in styles)
        {
            Assert.True(SectorStatePalette.ContrastRatio(s.Foreground, s.Background) >= 7.0, s.Tag);
        }
        // Los no fiables se marcan como relleno también en el texto.
        Assert.Contains("RELLENO", SectorStatePalette.For(SectorState.Unreadable).Tag);
        Assert.Contains("RELLENO", SectorStatePalette.For(SectorState.Skipped).Tag);
        Assert.Equal(21.0, SectorStatePalette.ContrastRatio(0x000000, 0xFFFFFF), 3);
    }
}
