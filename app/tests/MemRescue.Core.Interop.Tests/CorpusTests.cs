using System.Text;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>
/// Pruebas contra el corpus sintético (tests/corpus). Requieren las imágenes generadas
/// (scripts/test.ps1 las genera antes de ejecutar las pruebas).
/// </summary>
[Trait("Category", "Corpus")]
public sealed class CorpusTests
{
    private static string RepoRoot()
    {
        var d = new DirectoryInfo(AppContext.BaseDirectory);
        while (d is not null && !File.Exists(Path.Combine(d.FullName, "global.json")))
        {
            d = d.Parent;
        }
        return d?.FullName ?? throw new InvalidOperationException("No se encontró la raíz del repositorio.");
    }

    public static TheoryData<string> Manifests()
    {
        var data = new TheoryData<string>();
        foreach (var f in Directory.GetFiles(Path.Combine(RepoRoot(), "tests", "corpus", "manifests"), "*.manifest.toml").Order())
        {
            data.Add(Path.GetFileName(f));
        }
        return data;
    }

    private static Dictionary<string, string> TopLevel(string manifest) =>
        File.ReadLines(manifest)
            .TakeWhile(l => !l.StartsWith('['))
            .Select(l => l.Split(" = ", 2))
            .Where(p => p.Length == 2)
            .ToDictionary(p => p[0].Trim(), p => p[1].Trim().Trim('"'));

    private static List<(ulong Lba, string Marker)> Markers(string manifest)
    {
        var res = new List<(ulong, string)>();
        ulong? lba = null;
        foreach (var l in File.ReadLines(manifest))
        {
            if (l.StartsWith("start_lba = ", StringComparison.Ordinal))
            {
                lba = ulong.Parse(l["start_lba = ".Length..]);
            }
            else if (l.StartsWith("marker = ", StringComparison.Ordinal) && lba is { } v)
            {
                res.Add((v, l["marker = ".Length..].Trim('"')));
                lba = null;
            }
        }
        return res;
    }

    private static (Dictionary<string, string> M, string Img, string ManifestPath) Load(string manifestName)
    {
        var root = RepoRoot();
        var mp = Path.Combine(root, "tests", "corpus", "manifests", manifestName);
        var m = TopLevel(mp);
        var img = Path.Combine(root, "tests", "corpus", "generated", m["file"]);
        Assert.True(File.Exists(img), $"Falta {img}: ejecute scripts/test.ps1 o el generador.");
        return (m, img, mp);
    }

    [Theory]
    [MemberData(nameof(Manifests))]
    public async Task EngineThroughDllMatchesIndependentManifest(string manifestName)
    {
        var (m, img, _) = Load(manifestName);
        var info = Engine.GetImageInfo(img, uint.Parse(m["sector_size"]));
        Assert.Equal(ulong.Parse(m["size_bytes"]), info.SizeBytes);
        Assert.Equal(ulong.Parse(m["total_sectors"]), info.SectorCount);

        using var t = Engine.StartHash(img, HashAlgorithms.Sha256, uint.Parse(m["sector_size"]));
        Assert.Equal(EngineTaskState.Completed, await t.WaitAsync());
        Assert.Equal(m["sha256"], t.GetResult(HashAlgorithms.Sha256));
    }

    [Theory]
    [MemberData(nameof(Manifests))]
    public async Task SectorReadsHitTheExactLbaAndVerifyIsClean(string manifestName)
    {
        var (m, img, mp) = Load(manifestName);
        var ss = uint.Parse(m["sector_size"]);
        Assert.Equal(ImageFormat.Raw, Engine.Probe(img));
        using var src = Engine.OpenSource(img, ss);
        foreach (var (lba, marker) in Markers(mp))
        {
            var sector = src.ReadSectors(lba, 1);
            Assert.Equal(marker, Encoding.ASCII.GetString(sector, 0, marker.Length));
        }
        if (m["scheme"] == "gpt")
        {
            Assert.Equal("EFI PART", Encoding.ASCII.GetString(src.ReadSectors(1, 1), 0, 8));
            var last = src.ReadSectors(src.Info.SectorCount - 1, 1);
            Assert.Equal("EFI PART", Encoding.ASCII.GetString(last, 0, 8));
        }
        using var v = src.StartVerify();
        Assert.Equal(EngineTaskState.Completed, await v.WaitAsync());
        var r = v.GetReport();
        Assert.Equal(src.Info.SectorCount, r.SectorsOk);
        Assert.Equal(0ul, r.SectorsBad);
    }
}
