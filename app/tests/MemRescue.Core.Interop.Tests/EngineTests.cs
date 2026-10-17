using System.Runtime.InteropServices;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>Pruebas de la frontera C# ↔ Rust contra la memrescue_core.dll real.</summary>
public sealed class EngineTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-interop-").FullName;

    public void Dispose() => Directory.Delete(_dir, recursive: true);

    private string TempFile(string name, byte[] content)
    {
        var p = Path.Combine(_dir, name);
        File.WriteAllBytes(p, content);
        return p;
    }

    [Fact]
    public void AbiVersionMatchesAndStructLayoutsMatchHeader()
    {
        Assert.Equal(1u, Engine.AbiVersion);
        Engine.EnsureCompatible();
        Assert.Equal(32, Marshal.SizeOf<MrImageInfo>());
        Assert.Equal(24, Marshal.SizeOf<MrTaskStatus>());
    }

    [Fact]
    public void EngineVersionIsReadThroughOutputStringProtocol()
    {
        Assert.Equal("0.1.0", Engine.EngineVersion);
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
    [InlineData(@"\\?\GLOBALROOT\Device\Harddisk0\Partition0")]
    public void PhysicalDevicePathsAreRejected(string path)
    {
        var ex = Assert.Throws<EngineException>(() => Engine.GetImageInfo(path));
        Assert.Equal(EngineStatus.PathRejected, ex.Status);
        Assert.Contains("no permitida", ex.Message);
    }

    [Fact]
    public void MissingFileGivesIoErrorWithMessage()
    {
        var ex = Assert.Throws<EngineException>(() => Engine.GetImageInfo(Path.Combine(_dir, "no-existe.img")));
        Assert.Equal(EngineStatus.Io, ex.Status);
        Assert.False(string.IsNullOrWhiteSpace(ex.Message));
    }

    [Fact]
    public void InvalidSectorSizeIsAnIoError()
    {
        var p = TempFile("y.img", new byte[1024]);
        var ex = Assert.Throws<EngineException>(() => Engine.GetImageInfo(p, 500));
        Assert.Equal(EngineStatus.Io, ex.Status);
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
    public void DisposedTaskCannotBeUsed()
    {
        var p = TempFile("d.img", new byte[512]);
        var t = Engine.StartHash(p, HashAlgorithms.Crc32);
        t.Dispose();
        t.Dispose(); // idempotente
        Assert.Throws<ObjectDisposedException>(() => t.Status());
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
        // El contenido de la base se verifica en las pruebas Rust (mr-store, mr-api);
        // aquí solo que la ruta UTF-8 llega correctamente al motor.
        Assert.True(new FileInfo(db).Length > 0);
    }

    [Fact]
    public void Utf8PathsWithNonAsciiCharactersWork()
    {
        var p = TempFile("imagen-ñandú-测试.img", new byte[1024]);
        Assert.Equal(2ul, Engine.GetImageInfo(p).SectorCount);
    }

    /// <summary>IProgress síncrono (Progress&lt;T&gt; publica en el contexto, no apto para pruebas).</summary>
    private sealed class SyncProgress(Action<TaskProgress> a) : IProgress<TaskProgress>
    {
        public void Report(TaskProgress value) => a(value);
    }
}
