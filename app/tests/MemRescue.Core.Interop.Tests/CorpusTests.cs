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

    [Theory]
    [MemberData(nameof(Manifests))]
    public async Task EngineThroughDllMatchesIndependentManifest(string manifestName)
    {
        var root = RepoRoot();
        var m = TopLevel(Path.Combine(root, "tests", "corpus", "manifests", manifestName));
        var img = Path.Combine(root, "tests", "corpus", "generated", m["file"]);
        Assert.True(File.Exists(img), $"Falta {img}: ejecute scripts/test.ps1 o el generador.");

        var info = Engine.GetImageInfo(img, uint.Parse(m["sector_size"]));
        Assert.Equal(ulong.Parse(m["size_bytes"]), info.SizeBytes);
        Assert.Equal(ulong.Parse(m["total_sectors"]), info.SectorCount);

        using var t = Engine.StartHash(img, HashAlgorithms.Sha256, uint.Parse(m["sector_size"]));
        Assert.Equal(EngineTaskState.Completed, await t.WaitAsync());
        Assert.Equal(m["sha256"], t.GetResult(HashAlgorithms.Sha256));
    }
}
