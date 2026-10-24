using System.Runtime.InteropServices;
using System.Security.Cryptography;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>
/// Árbol de ficheros NTFS (#22) a través de la DLL real contra el corpus NTFS
/// (<c>tests/corpus/ntfs/generated</c>, descargado con <c>python -I -X utf8 tools/fetch_ntfs_corpus.py</c>;
/// <c>MR_NTFS_CORPUS</c> para otra ubicación).
/// </summary>
[Trait("Category", "NtfsCorpus")]
public sealed class NtfsTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-interop-ntfs-").FullName;

    public void Dispose()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        Directory.Delete(_dir, recursive: true);
    }

    private static string RepoRoot()
    {
        var d = new DirectoryInfo(AppContext.BaseDirectory);
        while (d is not null && !File.Exists(Path.Combine(d.FullName, "global.json")))
        {
            d = d.Parent;
        }
        return d?.FullName ?? throw new InvalidOperationException("No se encontró la raíz del repositorio.");
    }

    private static string Image(string name)
    {
        var dir = Environment.GetEnvironmentVariable("MR_NTFS_CORPUS") is { Length: > 0 } e
            ? e
            : Path.Combine(RepoRoot(), "tests", "corpus", "ntfs", "generated");
        var p = Path.Combine(dir, name + ".img");
        Assert.True(File.Exists(p), $"falta {p}: descargue el corpus con python -I -X utf8 tools/fetch_ntfs_corpus.py");
        return p;
    }

    /// <summary>Entrada <c>[[files]]</c> del manifiesto.</summary>
    private sealed record ManifestFile(string Path, string Type, ulong Record, string State, string? Sha256, ulong Size, bool Damaged);

    private static (bool Partitioned, List<ManifestFile> Files) Manifest(string name)
    {
        var path = Path.Combine(RepoRoot(), "tests", "corpus", "ntfs", "manifests", name + ".manifest.toml");
        var files = new List<ManifestFile>();
        Dictionary<string, string>? cur = null;
        var partitioned = false;
        void Flush()
        {
            if (cur is null)
            {
                return;
            }
            files.Add(new ManifestFile(cur["path"], cur["type"], ulong.Parse(cur["mft_record"]), cur["state"],
                cur.GetValueOrDefault("sha256"), cur.TryGetValue("size", out var s) ? ulong.Parse(s) : 0,
                cur.GetValueOrDefault("mft_damaged") == "true"));
            cur = null;
        }
        foreach (var raw in File.ReadLines(path))
        {
            var l = raw.Trim();
            if (l.StartsWith('['))
            {
                Flush();
                partitioned |= l == "[partition]";
                if (l == "[[files]]")
                {
                    cur = [];
                }
                continue;
            }
            if (cur is not null && l.Split(" = ", 2) is [var k, var v])
            {
                cur[k] = v.Trim('"');
            }
        }
        Flush();
        return (partitioned, files);
    }

    public static TheoryData<string> Manifests()
    {
        var data = new TheoryData<string>();
        foreach (var f in Directory.GetFiles(Path.Combine(RepoRoot(), "tests", "corpus", "ntfs", "manifests"), "*.manifest.toml").Order())
        {
            data.Add(Path.GetFileName(f)[..^".manifest.toml".Length]);
        }
        return data;
    }

    /// <summary>Volumen del manifiesto: la imagen o su partición 1.</summary>
    private static (Source Image, Source? Partition, NtfsVolume Volume) Open(string name, bool partitioned)
    {
        var img = Engine.OpenSource(Image(name));
        if (!partitioned)
        {
            return (img, null, img.OpenNtfs());
        }
        using var table = img.ReadPartitions();
        var part = table.OpenPartition(0);
        return (img, part, part.OpenNtfs());
    }

    [Fact]
    public void Ntfs_struct_layouts_match_header()
    {
        Assert.Equal(72, Marshal.SizeOf<MrNtfsVolumeInfo>());
        Assert.Equal(104, Marshal.SizeOf<MrNtfsEntry>());
        Assert.Equal(32, Marshal.SizeOf<MrNtfsListingInfo>());
        Assert.Equal(40, Marshal.SizeOf<MrNtfsTreeInfo>());
        Assert.Equal(40, Marshal.SizeOf<MrNtfsOrphan>());
        Assert.Equal(56, Marshal.SizeOf<MrNtfsCopyReport>());
        Assert.Equal(64, Marshal.OffsetOf<MrNtfsVolumeInfo>(nameof(MrNtfsVolumeInfo.MftLcn)).ToInt32());
        Assert.Equal(80, Marshal.OffsetOf<MrNtfsEntry>(nameof(MrNtfsEntry.FileAttributes)).ToInt32());
        Assert.Equal(100, Marshal.OffsetOf<MrNtfsEntry>(nameof(MrNtfsEntry.FoundSequence)).ToInt32());
    }

    [Theory]
    [MemberData(nameof(Manifests))]
    public async Task Tree_equals_the_live_paths_of_the_manifest(string name)
    {
        var (partitioned, files) = Manifest(name);
        var (img, part, vol) = Open(name, partitioned);
        using (img)
        using (part)
        using (vol)
        {
            Assert.True(vol.Info.UpcaseFromVolume);
            using var task = vol.StartTree();
            Assert.Equal(EngineTaskState.Completed, await task.WaitAsync());
            var tree = task.GetResult();
            Assert.Equal(NtfsTreeStatus.Complete, tree.Status);
            Assert.Empty(tree.Anomalies);
            Assert.Empty(tree.Orphans);
            Assert.Equal("/", tree.Nodes[0].Path);
            var got = tree.Nodes.Skip(1).Where(n => n.Entry.IsLive && !n.Entry.IsSystem)
                .Select(n => $"{n.Path}|{(n.Entry.IsDirectory ? "dir" : "file")}|{n.Entry.Record}").Order().ToList();
            var want = files.Where(f => f.State == "live" && !f.Damaged)
                .Select(f => $"{f.Path}|{f.Type}|{f.Record}").Order().ToList();
            Assert.Equal(want, got);
            var damaged = tree.Nodes.Where(n => n.Entry.State == NtfsEntryState.RecordUnusable).Select(n => n.Path).Order();
            Assert.Equal(files.Where(f => f.Damaged).Select(f => f.Path).Order(), damaged);
        }
    }

    [Fact]
    public async Task Paths_resolve_and_files_open_as_sources_and_copy()
    {
        var (_, files) = Manifest("ntfs_mbr");
        var (img, part, vol) = Open("ntfs_mbr", true);
        using (img)
        using (part)
        using (vol)
        {
            Assert.Equal("MR_MBR", vol.Info.Label);
            foreach (var f in files.Where(f => f.State == "live"))
            {
                var e = vol.Resolve(f.Path.ToUpperInvariant());
                Assert.Equal(f.Record, e.Record);
                Assert.Equal(f.Path, vol.PathOf(f.Record));
                if (f.Type == "file")
                {
                    Assert.Equal(f.Size, e.Size);
                }
            }
            var ex = Assert.Throws<EngineException>(() => vol.Resolve("/no/existe.txt"));
            Assert.Equal(EngineStatus.NotFound, ex.Status);
            // Listado de un directorio con nombres acentuados.
            var docs = vol.Resolve("/docs");
            var listing = vol.ListDirectory(docs.Record);
            Assert.Empty(listing.Anomalies);
            Assert.Contains(listing.Entries, e => e.Name == "índice.txt" && e.IsLive);
            Assert.All(listing.Entries, e => Assert.Equal(docs.Record, e.ParentRecord));

            // Un fichero no residente como origen y su copia.
            var target = files.First(f => f.Path == "/docs/proyectos/alfa/diseño.bin");
            using (var s = vol.OpenStream(target.Record))
            {
                Assert.Equal(512u, s.Info.SectorSize);
                Assert.Equal((target.Size + 511) / 512 * 512, s.Info.SizeBytes);
                var data = new List<byte>();
                for (ulong lba = 0; lba < s.Info.SectorCount; lba += 64)
                {
                    var page = s.ReadSectorsMapped(lba, (uint)Math.Min(64, s.Info.SectorCount - lba));
                    Assert.True(page.AllTrustworthy);
                    data.AddRange(page.Data);
                }
                Assert.Equal(target.Sha256, Convert.ToHexStringLower(SHA256.HashData(data.Take((int)target.Size).ToArray())));
                Assert.Contains("diseño.bin", s.Description);
            }
            var output = Path.Combine(_dir, "diseño copia.bin");
            using (var copy = vol.StartCopy(target.Record, output))
            {
                Assert.Equal(EngineTaskState.Completed, await copy.WaitAsync());
                var r = copy.GetReport();
                Assert.True(r.AllTrustworthy);
                Assert.Equal(target.Size, r.Bytes);
            }
            Assert.Equal(target.Sha256, Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(output))));
            // Nunca sobrescribe sin petición ni escribe sobre el origen.
            var exists = Assert.Throws<EngineException>(() => vol.StartCopy(target.Record, output));
            Assert.Equal(EngineStatus.AlreadyExists, exists.Status);
            var onSource = Assert.Throws<EngineException>(() => vol.StartCopy(target.Record, img.Path, overwrite: true));
            Assert.Equal(EngineStatus.InvalidArgument, onSource.Status);
            using (var again = vol.StartCopy(files.First(f => f.Path == "/LEEME.txt").Record, output, overwrite: true))
            {
                Assert.Equal(EngineTaskState.Completed, await again.WaitAsync());
            }
            Assert.Equal(1234, new FileInfo(output).Length);
            Assert.Equal(["diseño copia.bin"], Directory.GetFiles(_dir).Select(Path.GetFileName));
        }
    }

    [Fact]
    public void Entries_pointing_to_damaged_records_are_reported()
    {
        var (_, files) = Manifest("ntfs_damaged_mft");
        var (img, _, vol) = Open("ntfs_damaged_mft", false);
        using (img)
        using (vol)
        {
            foreach (var f in files.Where(f => f.Damaged))
            {
                var dir = vol.Resolve(f.Path[..f.Path.LastIndexOf('/')] is { Length: > 0 } p ? p : "/");
                var e = vol.ListDirectory(dir.Record).Entries.Single(x => x.Record == f.Record);
                Assert.Equal(NtfsEntryState.RecordUnusable, e.State);
                Assert.Equal(8u, e.RecordState);
                Assert.False(e.IsLive);
                Assert.Equal(EngineStatus.NotFound, Assert.Throws<EngineException>(() => vol.Resolve(f.Path)).Status);
            }
        }
    }
}
