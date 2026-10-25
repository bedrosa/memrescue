using System.Runtime.InteropServices;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>
/// Árbol virtual (#25) a través de la DLL real contra el corpus NTFS
/// (<c>tests/corpus/ntfs/generated</c>, descargado con <c>python -I -X utf8 tools/fetch_ntfs_corpus.py</c>;
/// <c>MR_NTFS_CORPUS</c> para otra ubicación).
/// </summary>
[Trait("Category", "NtfsCorpus")]
public sealed class VfsTests
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

    private static string Image(string name)
    {
        var dir = Environment.GetEnvironmentVariable("MR_NTFS_CORPUS") is { Length: > 0 } e
            ? e
            : Path.Combine(RepoRoot(), "tests", "corpus", "ntfs", "generated");
        var p = Path.Combine(dir, name + ".img");
        Assert.True(File.Exists(p), $"falta {p}: descargue el corpus con python -I -X utf8 tools/fetch_ntfs_corpus.py");
        return p;
    }

    /// <summary>Entradas <c>[[files]]</c> del manifiesto: ruta, tipo, estado y recuperabilidad.</summary>
    private static List<Dictionary<string, string>> Files(string name)
    {
        var path = Path.Combine(RepoRoot(), "tests", "corpus", "ntfs", "manifests", name + ".manifest.toml");
        var files = new List<Dictionary<string, string>>();
        Dictionary<string, string>? cur = null;
        foreach (var raw in File.ReadLines(path))
        {
            var l = raw.Trim();
            if (l.StartsWith('['))
            {
                cur = null;
                if (l == "[[files]]")
                {
                    cur = [];
                    files.Add(cur);
                }
                continue;
            }
            if (cur is not null && l.Split(" = ", 2) is [var k, var v])
            {
                cur[k] = v.Trim('"');
            }
        }
        return files;
    }

    private static async Task<VirtualFileSystem> Build(string name, VfsBuildOptions? options = null)
    {
        using var img = Engine.OpenSource(Image(name));
        using var vol = img.OpenNtfs();
        using var task = vol.StartVirtualFileSystem(options);
        var progress = new List<TaskProgress>();
        Assert.Equal(EngineTaskState.Completed, await task.WaitAsync(new SyncProgress(progress), pollInterval: TimeSpan.FromMilliseconds(5)));
        Assert.Equal(new TaskProgress(3000, 3000), task.Status().Progress);
        return task.GetResult();
    }

    private sealed class SyncProgress(List<TaskProgress> list) : IProgress<TaskProgress>
    {
        public void Report(TaskProgress value) => list.Add(value);
    }

    [Fact]
    public void Vfs_struct_layouts_match_header()
    {
        Assert.Equal(32, Marshal.SizeOf<MrVfsBuildOptions>());
        Assert.Equal(176, Marshal.SizeOf<MrVfsInfo>());
        Assert.Equal(128, Marshal.SizeOf<MrVfsNode>());
        Assert.Equal(56, Marshal.SizeOf<MrVfsFilter>());
        Assert.Equal(48, Marshal.OffsetOf<MrVfsInfo>(nameof(MrVfsInfo.Live)).ToInt32());
        Assert.Equal(152, Marshal.OffsetOf<MrVfsInfo>(nameof(MrVfsInfo.MemoryBytes)).ToInt32());
        Assert.Equal(96, Marshal.OffsetOf<MrVfsNode>(nameof(MrVfsNode.Record)).ToInt32());
        Assert.Equal(48, Marshal.OffsetOf<MrVfsFilter>(nameof(MrVfsFilter.TimeField)).ToInt32());
    }

    [Theory]
    [InlineData("ntfs_deleted")]
    [InlineData("ntfs_overwritten")]
    public async Task Every_manifest_entry_is_in_place_with_its_state(string name)
    {
        using var vfs = await Build(name);
        Assert.False(vfs.Info.Partial);
        var files = Files(name);
        foreach (var f in files)
        {
            var n = vfs.Find(f["path"]);
            Assert.True(n is not null, $"falta {f["path"]}");
            Assert.Equal(f["path"].Split('/')[^1], n.Name);
            Assert.Equal(ulong.Parse(f["mft_record"]), n.Record);
            var dir = f["type"] == "dir";
            Assert.Equal(dir ? VfsNodeKind.Directory : VfsNodeKind.File, n.Kind);
            if (f["state"] == "live")
            {
                Assert.Equal(VfsCategory.Live, n.Category);
                if (!dir)
                {
                    Assert.Equal(VfsDataState.Integro, n.DataState);
                    Assert.True(n.IsIntact);
                    Assert.Equal(ulong.Parse(f["size"]), n.Size);
                }
                continue;
            }
            Assert.Equal(VfsCategory.Deleted, n.Category);
            Assert.True(n.IsDeleted);
            Assert.False(n.IsIntact);
            Assert.Equal(VfsNameReliability.Record, n.NameReliability);
            if (!dir)
            {
                var expected = f["recoverable"] switch
                {
                    "full" => VfsDataState.IntegroNoVerificable,
                    "partial" => VfsDataState.Parcial,
                    _ => VfsDataState.Sobrescrito,
                };
                Assert.Equal(expected, n.DataState);
            }
        }
        var deleted = (ulong)files.Count(f => f["state"] != "live");
        Assert.Equal(deleted, vfs.Info.ByCategory[VfsCategory.Deleted]);
        Assert.Equal(deleted, vfs.Info.Deleted);
        // Metadatos en su carpeta sintética.
        var meta = vfs.Find("/[Metadatos]");
        Assert.NotNull(meta);
        Assert.Equal(VfsNodeKind.Synthetic, meta.Kind);
        Assert.Null(meta.Record);
        var mft = vfs.Find("/[Metadatos]/$MFT");
        Assert.NotNull(mft);
        Assert.Equal(VfsCategory.Metadata, mft.Category);
        Assert.DoesNotContain(vfs.GetChildren(), c => c.Name.StartsWith('$'));
    }

    [Fact]
    public async Task Queries_filter_by_deletion_masks_and_state()
    {
        const string name = "ntfs_overwritten";
        using var vfs = await Build(name);
        var files = Files(name);
        var deleted = vfs.Query(new VfsFilter { DeletedOnly = true, FilesOnly = true })
            .Select(vfs.GetNode).Where(n => n.Kind == VfsNodeKind.File).Select(n => vfs.PathOf(n.Id)).Order().ToList();
        var expected = files.Where(f => f["state"] != "live" && f["type"] == "file").Select(f => f["path"]).Order().ToList();
        Assert.Equal(expected, deleted);
        var overwritten = vfs.Query(new VfsFilter { DataStates = [VfsDataState.Sobrescrito], FilesOnly = true })
            .Select(vfs.GetNode).Where(n => n.Kind == VfsNodeKind.File).ToList();
        Assert.Equal(files.Count(f => f.GetValueOrDefault("recoverable") is "none"), overwritten.Count);
        var noBin = vfs.Query(new VfsFilter { Masks = "!*.bin", Categories = [VfsCategory.Live], FilesOnly = true })
            .Select(vfs.GetNode).Where(n => n.Kind == VfsNodeKind.File).ToList();
        Assert.NotEmpty(noBin);
        Assert.DoesNotContain(noBin, n => n.Name.EndsWith(".bin", StringComparison.OrdinalIgnoreCase));
        // Solo los hijos directos de la raíz.
        var top = vfs.Query(new VfsFilter { Recursive = false });
        Assert.Equal(vfs.GetChildren().Select(c => c.Id), top);
        // Orden determinista: dos construcciones dan las mismas rutas.
        using var again = await Build(name);
        var all = vfs.Query(new VfsFilter()).Select(vfs.PathOf).ToList();
        Assert.Equal(all, again.Query(new VfsFilter()).Select(again.PathOf).ToList());
        Assert.Null(vfs.Find("/no/existe"));
        var e = Assert.Throws<EngineException>(() => vfs.Query(new VfsFilter { Masks = "!" }));
        Assert.Equal(EngineStatus.InvalidArgument, e.Status);
    }

    [Fact]
    public async Task Build_options_skip_deleted_and_limits_fail()
    {
        using var vfs = await Build("ntfs_deleted", new VfsBuildOptions { Deleted = false, Metadata = false });
        Assert.Equal(0ul, vfs.Info.Deleted);
        Assert.Null(vfs.Find("/[Metadatos]"));
        using var img = Engine.OpenSource(Image("ntfs_deleted"));
        using var vol = img.OpenNtfs();
        using var task = vol.StartVirtualFileSystem(new VfsBuildOptions { MaxNodes = 5 });
        Assert.Equal(EngineTaskState.Failed, await task.WaitAsync(pollInterval: TimeSpan.FromMilliseconds(5)));
        var e = Assert.Throws<EngineException>(() => task.GetResult());
        Assert.Equal(EngineStatus.TaskFailed, e.Status);
    }
}
