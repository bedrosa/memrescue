using System.Runtime.InteropServices;
using System.Security.Cryptography;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>
/// Recuperación (#26) a través de la DLL real contra el corpus NTFS (<c>tests/corpus/ntfs/generated</c>,
/// descargado con <c>python -I -X utf8 tools/fetch_ntfs_corpus.py</c>; <c>MR_NTFS_CORPUS</c> para otra
/// ubicación). Cada prueba escribe en un directorio temporal propio y comprueba que el origen no cambia.
/// </summary>
[Trait("Category", "NtfsCorpus")]
public sealed class RecoverTests : IDisposable
{
    private readonly string _temp = Path.Combine(Path.GetTempPath(), "mr-recover-tests-" + Guid.NewGuid().ToString("N"));

    public RecoverTests() => Directory.CreateDirectory(_temp);

    public void Dispose()
    {
        try
        {
            foreach (var f in Directory.EnumerateFiles(_temp, "*", SearchOption.AllDirectories))
            {
                File.SetAttributes(f, FileAttributes.Normal);
            }
            Directory.Delete(_temp, true);
        }
        catch (IOException)
        {
        }
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

    private static string Sha256(string path) => Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(path)));

    private sealed record Opened(Source Source, NtfsVolume Volume, VirtualFileSystem Vfs) : IDisposable
    {
        public void Dispose()
        {
            Vfs.Dispose();
            Volume.Dispose();
            Source.Dispose();
        }
    }

    private static async Task<Opened> Open(string name)
    {
        var src = Engine.OpenSource(Image(name));
        var vol = src.OpenNtfs();
        using var t = vol.StartVirtualFileSystem();
        Assert.Equal(EngineTaskState.Completed, await t.WaitAsync(pollInterval: TimeSpan.FromMilliseconds(5)));
        return new Opened(src, vol, t.GetResult());
    }

    private static async Task<RecoverReport> Run(Opened o, IReadOnlyList<uint> nodes, string dest, RecoverOptions? options = null)
    {
        using var t = o.Vfs.StartRecover(o.Volume, nodes, dest, options);
        var state = await t.WaitAsync(pollInterval: TimeSpan.FromMilliseconds(5));
        Assert.NotEqual(EngineTaskState.Running, state);
        var p = t.Progress();
        Assert.Equal(p.FilesTotal, p.FilesDone);
        return t.GetReport();
    }

    [Fact]
    public void Recover_struct_layouts_match_header()
    {
        Assert.Equal(48, Marshal.SizeOf<MrRecoverOptions>());
        Assert.Equal(56, Marshal.SizeOf<MrRecoverProgress>());
        Assert.Equal(40, Marshal.SizeOf<MrRecoverQuestion>());
        Assert.Equal(200, Marshal.SizeOf<MrRecoverReport>());
        Assert.Equal(24, (int)Marshal.OffsetOf<MrRecoverOptions>(nameof(MrRecoverOptions.SplitSize)));
        Assert.Equal(32, (int)Marshal.OffsetOf<MrRecoverOptions>(nameof(MrRecoverOptions.Fill)));
        Assert.Equal(72, (int)Marshal.OffsetOf<MrRecoverReport>(nameof(MrRecoverReport.WrittenIntegro)));
        Assert.Equal(184, (int)Marshal.OffsetOf<MrRecoverReport>(nameof(MrRecoverReport.Capabilities)));
    }

    [Fact]
    public async Task Live_files_reproduce_the_manifest_and_the_source_is_untouched()
    {
        var before = Sha256(Image("ntfs_basic"));
        using var o = await Open("ntfs_basic");
        var dest = Path.Combine(_temp, "basico");
        var r = await Run(o, [VirtualFileSystem.Root], dest, new RecoverOptions { Categories = [VfsCategory.Live] });
        Assert.Equal(RecoverStatus.Complete, r.Status);
        Assert.Equal(0, r.ExitCode);
        Assert.True(r.AllIntact);
        Assert.Equal(r.FilesWritten, r.WrittenIntegro);
        Assert.True(File.Exists(r.JournalPath), r.JournalPath);
        Assert.Contains("\"tipo\":\"fin\"", File.ReadAllText(r.JournalPath));
        var checkedFiles = 0;
        foreach (var f in Files("ntfs_basic").Where(f => f["state"] == "live" && f["type"] == "file"))
        {
            var p = Path.Combine(dest, f["path"].TrimStart('/').Replace('/', Path.DirectorySeparatorChar));
            Assert.Equal(f["sha256"], Sha256(p));
            checkedFiles++;
        }
        Assert.True(checkedFiles > 40);
        Assert.Equal(before, Sha256(Image("ntfs_basic")));
        // Reanudar: nada nuevo.
        var again = await Run(o, [VirtualFileSystem.Root], dest,
            new RecoverOptions { Categories = [VfsCategory.Live], Conflict = RecoverConflictPolicy.SkipIfIdentical });
        Assert.Equal(0UL, again.FilesWritten);
        Assert.Equal(r.FilesWritten, again.SkippedIdentical);
    }

    [Fact]
    public async Task Deleted_files_are_never_presented_as_intact()
    {
        using var o = await Open("ntfs_deleted");
        var dest = Path.Combine(_temp, "borrados");
        var r = await Run(o, [VirtualFileSystem.Root], dest, new RecoverOptions { DeletedOnly = true });
        Assert.Equal(3, r.ExitCode);
        Assert.Equal(0UL, r.WrittenIntegro);
        Assert.True(r.WrittenIntegroNoVerificable >= 8, r.ToString());
        foreach (var f in Files("ntfs_deleted").Where(f => f["state"] == "deleted" && f["type"] == "file"
                     && f["recoverable"] == "full"))
        {
            var p = Path.Combine(dest, f["path"].TrimStart('/').Replace('/', Path.DirectorySeparatorChar));
            Assert.Equal(f["sha256"], Sha256(p));
        }
    }

    [Fact]
    public async Task Overwritten_files_with_options_and_rejections()
    {
        using var o = await Open("ntfs_overwritten");
        var target = o.Vfs.Find("/sobrescritura")!;
        var dest = Path.Combine(_temp, "sobrescritos");
        var r = await Run(o, [target.Id], dest);
        Assert.Equal(3, r.ExitCode);
        Assert.True(r.SkippedState >= 2);
        Assert.True(r.MarkedIncomplete >= 1);
        var all = await Run(o, [target.Id], Path.Combine(_temp, "con_bytes"), new RecoverOptions
        {
            WriteStates = [VfsDataState.Integro, VfsDataState.IntegroNoVerificable, VfsDataState.Parcial, VfsDataState.Sobrescrito],
            IncludeReallocatedBytes = true,
        });
        Assert.True(all.WrittenSobrescrito >= 2, all.ToString());
        foreach (var f in Files("ntfs_overwritten").Where(f => f["state"] == "overwritten"))
        {
            var p = Path.Combine(_temp, "con_bytes", f["path"].TrimStart('/').Replace('/', Path.DirectorySeparatorChar));
            Assert.Equal(f["recovered_sha256"], Sha256(p));
        }
        // Destino prohibido, sustitución sin confirmar.
        var e = Assert.Throws<EngineException>(() => o.Vfs.StartRecover(o.Volume, [0], Image("ntfs_overwritten")));
        Assert.Equal(EngineStatus.InvalidArgument, e.Status);
        e = Assert.Throws<EngineException>(() => o.Vfs.StartRecover(o.Volume, [0], @"\\.\PhysicalDrive0"));
        Assert.Equal(EngineStatus.PathRejected, e.Status);
        e = Assert.Throws<EngineException>(() => o.Vfs.StartRecover(o.Volume, [0], dest,
            new RecoverOptions { Conflict = RecoverConflictPolicy.Overwrite }));
        Assert.Equal(EngineStatus.InvalidArgument, e.Status);
    }

    [Fact]
    public async Task Ask_policy_questions_are_answered_and_cancel_works()
    {
        using var o = await Open("ntfs_basic");
        var docs = o.Vfs.Find("/docs")!;
        var dest = Path.Combine(_temp, "preguntas");
        await Run(o, [docs.Id], dest);
        using (var t = o.Vfs.StartRecover(o.Volume, [docs.Id], dest, new RecoverOptions { Conflict = RecoverConflictPolicy.Ask }))
        {
            var asked = 0;
            while (t.Status().State == EngineTaskState.Running)
            {
                if (t.PendingQuestion() is { } q)
                {
                    Assert.True(Directory.Exists(q.Existing) || File.Exists(q.Existing));
                    Assert.False(string.IsNullOrEmpty(q.Requested));
                    Assert.True(t.Answer(q.Id, RecoverDecision.SkipIfIdentical, applyToAll: true));
                    asked++;
                }
                await Task.Delay(2);
            }
            var r = t.GetReport();
            Assert.Equal(0UL, r.FilesWritten);
            Assert.InRange(asked, 1, 2);
        }
        using (var t = o.Vfs.StartRecover(o.Volume, [docs.Id], dest, new RecoverOptions { Conflict = RecoverConflictPolicy.Ask }))
        {
            while (!t.Progress().QuestionPending)
            {
                await Task.Delay(2);
            }
            t.Cancel();
            Assert.Equal(EngineTaskState.Cancelled, await t.WaitAsync(pollInterval: TimeSpan.FromMilliseconds(5)));
            var r = t.GetReport();
            Assert.Equal(RecoverStatus.Cancelled, r.Status);
            Assert.Equal(0UL, r.FilesWritten);
        }
    }
}
