using System.Runtime.InteropServices;
using System.Text;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>Búsqueda exprés de particiones perdidas a través de la DLL real (issue #15).</summary>
public sealed class PartitionSearchTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-interop-search-").FullName;

    public void Dispose()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        Directory.Delete(_dir, recursive: true);
    }

    /// <summary>Sector de arranque NTFS sintético (total = sectores - 1; copia en el último sector).</summary>
    private static byte[] NtfsBoot(uint hidden, ulong sectors)
    {
        var b = new byte[512];
        b[0] = 0xEB; b[1] = 0x52; b[2] = 0x90;
        Encoding.ASCII.GetBytes("NTFS    ").CopyTo(b, 3);
        BitConverter.TryWriteBytes(b.AsSpan(0x0B, 2), (ushort)512);
        b[0x0D] = 8;
        b[0x15] = 0xF8;
        BitConverter.TryWriteBytes(b.AsSpan(0x1C, 4), hidden);
        BitConverter.TryWriteBytes(b.AsSpan(0x28, 8), sectors - 1);
        BitConverter.TryWriteBytes(b.AsSpan(0x30, 8), 4UL);
        BitConverter.TryWriteBytes(b.AsSpan(0x38, 8), sectors / 16);
        b[0x40] = 0xF6;
        b[0x44] = 1;
        BitConverter.TryWriteBytes(b.AsSpan(0x48, 8), 0x1234UL);
        b[0x1FE] = 0x55; b[0x1FF] = 0xAA;
        return b;
    }

    /// <summary>Disco sin tabla con dos NTFS (LBA 2048 y 4096 del disco), con <paramref name="added"/> sectores añadidos al inicio.</summary>
    private string LostImage(int added, string name)
    {
        var d = new byte[(6144 + added) * 512];
        foreach (var (start, len) in new[] { (2048, 2048), (4096, 2048) })
        {
            var b = NtfsBoot((uint)start, (ulong)len);
            b.CopyTo(d, (start + added) * 512);
            b.CopyTo(d, (start + added + len - 1) * 512);
        }
        var p = Path.Combine(_dir, name);
        File.WriteAllBytes(p, d);
        return p;
    }

    [Fact]
    public void StructLayoutsMatchHeader()
    {
        Assert.Equal(64, Marshal.SizeOf<MrPartitionSearchOptions>());
        Assert.Equal(96, Marshal.SizeOf<MrPartitionSearchInfo>());
        Assert.Equal(24, Marshal.OffsetOf<MrPartitionSearchInfo>(nameof(MrPartitionSearchInfo.ShiftBytes)).ToInt32());
        Assert.Equal(136, Marshal.SizeOf<MrFoundVolume>());
        Assert.Equal(96, Marshal.OffsetOf<MrFoundVolume>(nameof(MrFoundVolume.PrimaryByte)).ToInt32());
        Assert.Equal(128, Marshal.OffsetOf<MrFoundVolume>(nameof(MrFoundVolume.Score)).ToInt32());
        Assert.Equal(32, Marshal.SizeOf<MrFoundTable>());
    }

    [Fact]
    public async Task SearchFindsVolumesAndOpensThem()
    {
        var img = LostImage(0, "perdidos.img");
        var before = File.ReadAllBytes(img);
        using var src = Engine.OpenSource(img);
        using var task = src.StartPartitionSearch();
        var progress = new List<TaskProgress>();
        Assert.Equal(EngineTaskState.Completed, await task.WaitAsync(new Progress<TaskProgress>(progress.Add)));
        var r = task.GetResult();
        Assert.Equal(PartitionSearchStatus.Complete, r.Status);
        Assert.Equal(4096ul, r.MaxReads);
        Assert.InRange(r.Reads, 1ul, 400ul);
        Assert.Null(r.ShiftBytes);
        Assert.Equal(2, r.Volumes.Count);
        var v = r.Volumes[0];
        Assert.Equal((FoundFileSystem.Ntfs, 2048L, 2048ul), (v.FileSystem, v.StartLba, v.SectorCount));
        Assert.Equal(ConfidenceLevel.Valid, v.Confidence);
        Assert.Equal("Ex P+ C+ Sx U+ A+", v.Indicators);
        Assert.Equal("encontrada", v.StateLabel);
        Assert.Equal(0x1234ul, v.Serial);
        Assert.Equal((byte)7, v.SuggestedMbrType);
        Assert.Contains("1 MiB", v.Origin);
        using var vol = task.OpenVolume(0);
        Assert.Equal(2048ul, vol.Info.SectorCount);
        Assert.Equal(2048ul, vol.RangeStart);
        Assert.StartsWith("Volumen encontrado NTFS LBA 2048", vol.Description);
        Assert.Equal("NTFS    ", Encoding.ASCII.GetString(vol.ReadSectors(0, 1), 3, 8));
        Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => task.OpenVolume(5)).Status);
        Assert.Equal(before, File.ReadAllBytes(img));
    }

    [Fact]
    public async Task ShiftIsReportedAndAppliedOnlyOnRequest()
    {
        using var src = Engine.OpenSource(LostImage(16, "desplazada.img"));
        using var task = src.StartPartitionSearch();
        await task.WaitAsync();
        var r = task.GetResult();
        Assert.Equal(16L * 512, r.ShiftBytes);
        var a = Assert.Single(r.Anomalies);
        Assert.Equal("image_shifted", a.Name);
        Assert.True(a.LbaSigned);
        Assert.Equal(16L, a.SignedLba);
        Assert.Equal(-16L, a.SignedValue);
        var v = r.Volumes[0];
        Assert.True(v.RequiresShift);
        Assert.Equal(ConfidenceLevel.Suspicious, v.Confidence);
        Assert.Contains(v.Anomalies, x => x.Name == "found_shift_detected");
        var e = Assert.Throws<EngineException>(() => task.OpenVolume(0));
        Assert.Equal(EngineStatus.InvalidArgument, e.Status);
        Assert.Contains("desplazada 16 sectores", e.Message);
        using var vol = task.OpenVolume(0, applyShift: true);
        Assert.Contains("desplazamiento detectado aplicado", vol.Description);
    }

    [Fact]
    public async Task OptionsAreValidatedAndBudgetIsReported()
    {
        using var src = Engine.OpenSource(LostImage(0, "presupuesto.img"));
        Assert.Equal(EngineStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => src.StartPartitionSearch(new PartitionSearchOptions { ProbeWindow = 1000 })).Status);
        using var t = src.StartPartitionSearch(new PartitionSearchOptions { MaxReads = 2 });
        Assert.Equal(EngineTaskState.Completed, await t.WaitAsync());
        var r = t.GetResult();
        Assert.Equal(PartitionSearchStatus.BudgetExhausted, r.Status);
        Assert.True(r.Reads <= 2 && r.Pending > 0);
        using var t2 = src.StartPartitionSearch(new PartitionSearchOptions { Families = SearchFamilies.ExFat });
        await t2.WaitAsync();
        Assert.Empty(t2.GetResult().Volumes);
    }

    [Fact]
    public async Task CancelledSearchKeepsItsPartialResult()
    {
        var p = Path.Combine(_dir, "grande.img");
        using (var f = File.Create(p))
        {
            f.SetLength(2L << 30);
        }
        using var src = Engine.OpenSource(p);
        using var t = src.StartPartitionSearch();
        using var cts = new CancellationTokenSource();
        cts.Cancel();
        var state = await t.WaitAsync(cancellationToken: cts.Token);
        var r = t.GetResult();
        Assert.Equal(state == EngineTaskState.Cancelled ? PartitionSearchStatus.Cancelled : PartitionSearchStatus.Complete, r.Status);
        Assert.Empty(r.Volumes);
    }
}

/// <summary>La búsqueda contra los <c>[[found_volumes]]</c> de cada manifiesto del corpus.</summary>
[Trait("Category", "Corpus")]
public sealed class PartitionSearchCorpusTests
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

    private static string State(EvidenceCheck c) => c switch
    {
        EvidenceCheck.Yes => "valid",
        EvidenceCheck.No => "damaged",
        EvidenceCheck.Unverifiable => "unreadable",
        EvidenceCheck.Outside => "outside_image",
        _ => "?",
    };

    [Theory]
    [MemberData(nameof(Manifests))]
    public async Task FoundVolumesMatchTheManifest(string manifestName)
    {
        var mp = Path.Combine(RepoRoot(), "tests", "corpus", "manifests", manifestName);
        var top = new Dictionary<string, string>();
        var volumes = new List<Dictionary<string, string>>();
        Dictionary<string, string>? current = null;
        var section = "";
        foreach (var l in File.ReadLines(mp))
        {
            if (l.StartsWith('['))
            {
                section = l;
                current = l == "[[found_volumes]]" ? new Dictionary<string, string>() : null;
                if (current is not null)
                {
                    volumes.Add(current);
                }
                continue;
            }
            var kv = l.Split(" = ", 2);
            if (kv.Length != 2 || l.StartsWith('#'))
            {
                continue;
            }
            (section == "" ? top : current)?.TryAdd(kv[0], kv[1].Trim('"'));
        }
        if (top.ContainsKey("unreadable_lbas"))
        {
            return; // un fichero RAW no tiene sectores ilegibles (ver core/mr-part/tests/search_corpus.rs)
        }
        var img = Path.Combine(RepoRoot(), "tests", "corpus", "generated", top["file"]);
        Assert.True(File.Exists(img), $"Falta {img}: ejecute scripts/test.ps1 o el generador.");
        using var src = Engine.OpenSource(img, uint.Parse(top["sector_size"]));
        using var t = src.StartPartitionSearch();
        Assert.Equal(EngineTaskState.Completed, await t.WaitAsync());
        var r = t.GetResult();
        Assert.Equal(PartitionSearchStatus.Complete, r.Status);
        Assert.Equal(volumes.Count, r.Volumes.Count);
        for (var i = 0; i < volumes.Count; i++)
        {
            var e = volumes[i];
            var v = r.Volumes[i];
            Assert.Equal(e["fs"], v.FileSystem.Label().ToLowerInvariant());
            Assert.Equal(long.Parse(e["start_lba"]), v.StartLba);
            Assert.Equal(ulong.Parse(e["sectors"]), v.SectorCount);
            Assert.Equal(e["primary_boot"], State(v.Primary));
            Assert.Equal(e["backup_boot"], State(v.Backup));
            Assert.Equal(bool.Parse(e["partial"]), v.Partial);
            Assert.Equal(ulong.Parse(e["hidden_sectors"]), v.DeclaredStart);
            if (!v.Partial && !v.RequiresShift)
            {
                using var vol = t.OpenVolume(i);
                Assert.Equal((ulong)v.StartLba, vol.RangeStart);
            }
        }
    }
}
