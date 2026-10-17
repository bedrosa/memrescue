using System.Runtime.InteropServices;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>Pruebas de la frontera C# ↔ Rust contra la memrescue_core.dll real.</summary>
public sealed class EngineTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-interop-").FullName;

    public void Dispose()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        Directory.Delete(_dir, recursive: true);
    }

    private string TempFile(string name, byte[] content)
    {
        var p = Path.Combine(_dir, name);
        File.WriteAllBytes(p, content);
        return p;
    }

    /// <summary>Imagen donde cada byte del sector n vale n (mod 256).</summary>
    private string Numbered(string name, int sectors, int sectorSize) =>
        TempFile(name, Enumerable.Range(0, sectors * sectorSize).Select(i => (byte)(i / sectorSize)).ToArray());

    [Fact]
    public void AbiVersionMatchesAndStructLayoutsMatchHeader()
    {
        Assert.Equal(3u, Engine.AbiVersion);
        Engine.EnsureCompatible();
        Assert.Equal(32, Marshal.SizeOf<MrImageInfo>());
        Assert.Equal(24, Marshal.SizeOf<MrTaskStatus>());
        Assert.Equal(72, Marshal.SizeOf<MrVerifyReport>());
    }

    [Fact]
    public void EngineVersionIsReadThroughOutputStringProtocol()
    {
        Assert.Equal("0.2.0", Engine.EngineVersion);
    }

    [Fact]
    public void ImageInfoReportsSizeSectorsAndTrailingBytes()
    {
        var p = TempFile("x.img", new byte[1500]);
        var i = Engine.GetImageInfo(p, 512);
        Assert.Equal(1500ul, i.SizeBytes);
        Assert.Equal(2ul, i.SectorCount);
        Assert.Equal(476ul, i.TrailingBytes);
        Assert.Equal(512u, i.SectorSize);
    }

    [Theory]
    [InlineData(@"\\.\PhysicalDrive0")]
    [InlineData(@"\\.\C:")]
    [InlineData(@"\\?\C:")]
    [InlineData(@"\\?\HarddiskVolume1")]
    [InlineData(@"\\?\GLOBALROOT\Device\Harddisk0\Partition0")]
    [InlineData("NUL")]
    public void PhysicalDevicePathsAreRejected(string path)
    {
        var ex = Assert.Throws<EngineException>(() => Engine.GetImageInfo(path));
        Assert.Equal(EngineStatus.PathRejected, ex.Status);
        Assert.Contains("no permitida", ex.Message);
        Assert.Equal(EngineStatus.PathRejected, Assert.Throws<EngineException>(() => Engine.OpenSource(path)).Status);
    }

    [Fact]
    public void MissingFileGivesIoErrorWithMessage()
    {
        var ex = Assert.Throws<EngineException>(() => Engine.GetImageInfo(Path.Combine(_dir, "no-existe.img")));
        Assert.Equal(EngineStatus.Io, ex.Status);
        Assert.False(string.IsNullOrWhiteSpace(ex.Message));
    }

    [Fact]
    public void InvalidSectorSizeIsRejected()
    {
        var p = TempFile("y.img", new byte[1024]);
        var ex = Assert.Throws<EngineException>(() => Engine.GetImageInfo(p, 500));
        Assert.Equal(EngineStatus.InvalidArgument, ex.Status);
    }

    [Theory]
    [InlineData(512u)]
    [InlineData(1024u)]
    [InlineData(2048u)]
    [InlineData(4096u)]
    public void SourceReadsExactSectorsForEachSectorSize(uint ss)
    {
        var p = Numbered($"s{ss}.img", 10, (int)ss);
        using var src = Engine.OpenSource(p, ss);
        Assert.Equal(10ul, src.Info.SectorCount);
        var b = src.ReadSectors(8, 2);
        Assert.Equal(2 * (int)ss, b.Length);
        Assert.All(b.Take((int)ss), x => Assert.Equal(8, x));
        Assert.All(b.Skip((int)ss), x => Assert.Equal(9, x));
        Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => src.ReadSectors(9, 2)).Status);
        Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => src.ReadSectors(ulong.MaxValue, 1)).Status);
    }

    [Fact]
    public void ContainersAreDetectedAndNotOpenedAsRawUnlessForced()
    {
        var bytes = new byte[4096];
        "vhdxfile"u8.CopyTo(bytes);
        var p = TempFile("d.vhdx", bytes);
        Assert.Equal(ImageFormat.Vhdx, Engine.Probe(p));
        // Firma VHDX sin estructuras válidas: se rechaza como dañado, nunca se lee como RAW por error.
        var ex = Assert.Throws<EngineException>(() => Engine.OpenSource(p));
        Assert.Equal(EngineStatus.Corrupt, ex.Status);
        Assert.Contains("VHDX", ex.Message);
        using var forced = Engine.OpenSource(p, 512, forceRaw: true);
        Assert.Equal(8ul, forced.Info.SectorCount);
        Assert.Equal(ImageFormat.Raw, forced.Info.Format);
        Assert.Contains("forzado", forced.Description);
        // Contenedor detectado sin lector (QCOW): formato no soportado.
        var q = new byte[4096];
        "QFI"u8.CopyTo(q);
        q[3] = 0xFB;
        Assert.Equal(EngineStatus.UnsupportedFormat,
            Assert.Throws<EngineException>(() => Engine.OpenSource(TempFile("d.qcow2", q))).Status);
        Assert.Equal(ImageFormat.Raw, Engine.Probe(Numbered("r.img", 2, 512)));
    }

    [Fact]
    public async Task HashTaskProducesKnownDigestsForAbc()
    {
        var p = TempFile("abc.img", "abc"u8.ToArray());
        using var t = Engine.StartHash(p, HashAlgorithms.Crc32 | HashAlgorithms.Md5 | HashAlgorithms.Sha1 | HashAlgorithms.Sha256);
        var reports = new List<TaskProgress>();
        var state = await t.WaitAsync(new SyncProgress(reports.Add));
        Assert.Equal(EngineTaskState.Completed, state);
        Assert.Equal("352441c2", t.GetResult(HashAlgorithms.Crc32));
        Assert.Equal("900150983cd24fb0d6963f7d28e17f72", t.GetResult(HashAlgorithms.Md5));
        Assert.Equal("a9993e364706816aba3e25717850c26c9cd0d89d", t.GetResult(HashAlgorithms.Sha1));
        Assert.Equal("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", t.GetResult(HashAlgorithms.Sha256));
        Assert.Contains(reports, r => r.Done == 3 && r.Total == 3);
    }

    [Fact]
    public async Task SourceHashMatchesPathHash()
    {
        var p = Numbered("h.img", 64, 512);
        using var src = Engine.OpenSource(p);
        using var a = src.StartHash(HashAlgorithms.Sha256);
        using var b = Engine.StartHash(p, HashAlgorithms.Sha256);
        Assert.Equal(EngineTaskState.Completed, await a.WaitAsync());
        Assert.Equal(EngineTaskState.Completed, await b.WaitAsync());
        Assert.Equal(b.GetResult(HashAlgorithms.Sha256), a.GetResult(HashAlgorithms.Sha256));
    }

    [Fact]
    public async Task VerifyReadsEverySectorWithProgressInSectors()
    {
        var p = Numbered("v.img", 3000, 512);
        using var src = Engine.OpenSource(p);
        using var t = src.StartVerify();
        TaskProgress last = default;
        Assert.Equal(EngineTaskState.Completed, await t.WaitAsync(new SyncProgress(x => last = x)));
        Assert.Equal(new TaskProgress(3000, 3000), last);
        var r = t.GetReport();
        Assert.Equal((3000ul, 3000ul, 0ul, 0ul), (r.SectorsRequested, r.SectorsRead, r.SectorsUnreadable, r.SectorsSkipped));
        Assert.True(r.AllTrustworthy);
        Assert.Empty(r.UntrustedRanges);
        Assert.Equal(EngineStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => src.StartVerify(retries: 1000)).Status);
    }

    [Fact]
    public async Task UnrequestedAlgorithmIsAnError()
    {
        var p = TempFile("abc2.img", "abc"u8.ToArray());
        using var t = Engine.StartHash(p, HashAlgorithms.Sha256);
        await t.WaitAsync();
        var ex = Assert.Throws<EngineException>(() => t.GetResult(HashAlgorithms.Md5));
        Assert.Equal(EngineStatus.InvalidArgument, ex.Status);
    }

    [Fact]
    public async Task CancellationIsHonouredAndReported()
    {
        var p = TempFile("big.img", new byte[256 * 1024 * 1024]);
        using var t = Engine.StartHash(p, HashAlgorithms.Sha256 | HashAlgorithms.Sha1 | HashAlgorithms.Md5);
        using var cts = new CancellationTokenSource();
        cts.Cancel();
        var state = await t.WaitAsync(cancellationToken: cts.Token, pollInterval: TimeSpan.FromMilliseconds(5));
        Assert.Equal(EngineTaskState.Cancelled, state);
        var ex = Assert.Throws<EngineException>(() => t.GetResult(HashAlgorithms.Sha256));
        Assert.Equal(EngineStatus.Cancelled, ex.Status);
        var (_, progress) = t.Status();
        Assert.True(progress.Done < progress.Total, "la cancelación debía llegar antes del final");
    }

    [Fact]
    public void DisposedHandlesCannotBeUsed()
    {
        var p = TempFile("d.img", new byte[512]);
        var t = Engine.StartHash(p, HashAlgorithms.Crc32);
        t.Dispose();
        t.Dispose(); // idempotente
        Assert.True(t.IsDisposed);
        Assert.Throws<ObjectDisposedException>(() => t.Status());
        var s = Engine.OpenSource(p);
        s.Dispose();
        Assert.Throws<ObjectDisposedException>(() => s.ReadSectors(0, 1));
    }

    /// <summary>Regresión: un handle no liberado explícitamente lo libera el finalizador sin errores.</summary>
    [Fact]
    public void ForgottenHandlesAreReleasedByFinalizer()
    {
        var p = TempFile("f.img", new byte[4096]);
        static void Leak(string path)
        {
            for (var i = 0; i < 200; i++)
            {
                _ = Engine.OpenSource(path);
            }
        }
        Leak(p);
        GC.Collect();
        GC.WaitForPendingFinalizers();
        // El motor sigue sano y acepta nuevos orígenes.
        using var s = Engine.OpenSource(p);
        Assert.Equal(8ul, s.Info.SectorCount);
    }

    [Fact]
    public async Task RecordsJobInWorkDatabaseWhenRequested()
    {
        var p = TempFile("j.img", new byte[4096]);
        var db = Path.Combine(_dir, "w.mrdb");
        using (var t = Engine.StartHash(p, HashAlgorithms.Sha256, storePath: db))
        {
            Assert.Equal(EngineTaskState.Completed, await t.WaitAsync());
        }
        Assert.True(File.Exists(db));
        Assert.True(new FileInfo(db).Length > 0);
    }

    [Fact]
    public void Utf8PathsWithNonAsciiCharactersWork()
    {
        var p = TempFile("imagen-ñandú-测试.img", new byte[1024]);
        Assert.Equal(2ul, Engine.GetImageInfo(p).SectorCount);
    }

    [Fact]
    public void SourceFileIsNeverModified()
    {
        var p = Numbered("ro.img", 128, 512);
        var before = File.ReadAllBytes(p);
        var mtime = File.GetLastWriteTimeUtc(p);
        using (var s = Engine.OpenSource(p))
        {
            for (ulong l = 0; l < 128; l += 7)
            {
                s.ReadSectors(l, 1);
            }
        }
        Assert.Equal(before, File.ReadAllBytes(p));
        Assert.Equal(mtime, File.GetLastWriteTimeUtc(p));
    }

    /// <summary>IProgress síncrono (Progress&lt;T&gt; publica en el contexto, no apto para pruebas).</summary>
    private sealed class SyncProgress(Action<TaskProgress> a) : IProgress<TaskProgress>
    {
        public void Report(TaskProgress value) => a(value);
    }
}
