using System.Buffers.Binary;
using System.Security.Cryptography;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>
/// ABI 3 desde C#: contenedores, rangos como origen, lectura con estado por sector e imágenes
/// divididas. Las imágenes se construyen en un directorio temporal y se comprueba que no cambian.
/// </summary>
public sealed class ContainerTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-abi3-").FullName;

    public void Dispose()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        Directory.Delete(_dir, recursive: true);
    }

    private static byte[] NumberedBytes(int sectors, int ss) =>
        Enumerable.Range(0, sectors * ss).Select(i => (byte)(i / ss)).ToArray();

    /// <summary>VHD fijo según la especificación pública: datos + pie de 512 bytes (big-endian).</summary>
    private static byte[] FixedVhd(byte[] data)
    {
        var f = new byte[512];
        "conectix"u8.CopyTo(f);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(8), 2);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(12), 0x0001_0000);
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(16), ulong.MaxValue);
        "mrts"u8.CopyTo(f.AsSpan(28));
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(40), (ulong)data.Length);
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(48), (ulong)data.Length);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(56), (16u << 16) | (4u << 8) | 17u);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(60), 2);
        uint sum = 0;
        for (var i = 0; i < 512; i++)
        {
            if (i is < 64 or >= 68)
            {
                sum += f[i];
            }
        }
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(64), ~sum);
        return [.. data, .. f];
    }

    private string Write(string name, byte[] content)
    {
        var p = Path.Combine(_dir, name);
        File.WriteAllBytes(p, content);
        return p;
    }

    [Fact]
    public void FixedVhdIsReadThroughTheContainerAndLeftIntact()
    {
        var data = NumberedBytes(64, 512);
        var p = Write("d.vhd", FixedVhd(data));
        var before = SHA256.HashData(File.ReadAllBytes(p));
        Assert.Equal(ImageFormat.Vhd, Engine.Probe(p));
        using (var src = Engine.OpenSource(p))
        {
            Assert.Equal(ImageFormat.Vhd, src.Info.Format);
            Assert.Equal(64ul, src.Info.SectorCount);
            Assert.Contains("VHD fijo", src.Description);
            Assert.All(src.ReadSectors(63, 1), b => Assert.Equal(63, b));
            // El pie no forma parte del disco virtual.
            Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => src.ReadSectors(64, 1)).Status);
            // Visto con sectores de 4096 B: mismos bytes, otra división.
            using var big = Engine.OpenSource(p, 4096);
            Assert.Equal(8ul, big.Info.SectorCount);
            Assert.Equal(src.ReadSectors(8, 8), big.ReadSectors(1, 1));
        }
        // Suma de control alterada: dañado (no se interpreta ni se lee como RAW).
        var bad = File.ReadAllBytes(p);
        bad[^1] ^= 0xFF;
        bad[bad.Length - 512 + 64] ^= 0x01;
        Assert.Equal(EngineStatus.Corrupt,
            Assert.Throws<EngineException>(() => Engine.OpenSource(Write("bad.vhd", bad))).Status);
        Assert.Equal(before, SHA256.HashData(File.ReadAllBytes(p)));
    }

    [Fact]
    public void SeparateVmdkDescriptorIsAnExternalReferenceThatIsNotFollowed()
    {
        var p = Write("flat.vmdk", System.Text.Encoding.ASCII.GetBytes(
            "# Disk DescriptorFile\nversion=1\nCID=1\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\n" +
            "RW 64 FLAT \"..\\..\\Windows\\win.ini\" 0\n"));
        var ex = Assert.Throws<EngineException>(() => Engine.OpenSource(p));
        Assert.Equal(EngineStatus.ExternalReference, ex.Status);
        Assert.Contains("win.ini", ex.Message);
    }

    [Fact]
    public void RangesAreIndependentBoundedSources()
    {
        var p = Write("r.img", NumberedBytes(100, 512));
        using var parent = Engine.OpenSource(p);
        using var r = parent.OpenRange(10, 20);
        Assert.Equal(20ul, r.Info.SectorCount);
        Assert.Equal(10ul, r.RangeStart);
        Assert.StartsWith("Rango LBA 10–29", r.Description);
        Assert.All(r.ReadSectors(0, 1), b => Assert.Equal(10, b));
        Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => r.ReadSectors(20, 1)).Status);
        Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => parent.OpenRange(90, 11)).Status);
        Assert.Equal(EngineStatus.OutOfRange, Assert.Throws<EngineException>(() => parent.OpenRange(0, 0)).Status);
        using var nested = r.OpenRange(5, 5);
        Assert.All(nested.ReadSectors(4, 1), b => Assert.Equal(19, b));
    }

    [Fact]
    public void MappedReadReportsAStatePerSector()
    {
        var p = Write("m.img", NumberedBytes(16, 512));
        using var src = Engine.OpenSource(p);
        var page = src.ReadSectorsMapped(12, 4);
        Assert.Equal(4 * 512, page.Data.Length);
        Assert.Equal([SectorState.Read, SectorState.Read, SectorState.Read, SectorState.Read], page.States);
        Assert.True(page.AllTrustworthy);
        Assert.All(page.Data.Take(512), b => Assert.Equal(12, b));
        Assert.Equal(EngineStatus.InvalidArgument, Assert.Throws<EngineException>(() => src.ReadSectorsMapped(0, 1, retries: 101)).Status);
        Assert.False(SectorState.Unreadable.IsTrustworthy());
        Assert.False(SectorState.Skipped.IsTrustworthy());
        Assert.True(SectorState.Sparse.IsTrustworthy());
        Assert.Contains("relleno", SectorState.Unreadable.Label());
    }

    [Fact]
    public async Task SplitImageIsListedThenOpenedOnlyWhenRequested()
    {
        var data = NumberedBytes(30, 512);
        for (var k = 0; k < 3; k++)
        {
            Write($"s.{k + 1:000}", data[(k * 5120)..((k + 1) * 5120)]);
        }
        var first = Path.Combine(_dir, "s.001");
        var segs = Engine.DiscoverSplit(first);
        Assert.Equal(3, segs.Count);
        Assert.EndsWith("s.003", segs[2]);
        using (var one = Engine.OpenSource(first))
        {
            Assert.Equal(10ul, one.Info.SectorCount);
        }
        using var all = Engine.OpenSource(first, split: true);
        Assert.Equal(30ul, all.Info.SectorCount);
        Assert.Contains("3 segmentos", all.Description);
        Assert.Equal(data[(9 * 512)..(11 * 512)], all.ReadSectors(9, 2));
        using var h = all.StartHash(HashAlgorithms.Sha256);
        Assert.Equal(EngineTaskState.Completed, await h.WaitAsync());
        Assert.Equal(Convert.ToHexStringLower(SHA256.HashData(data)), h.GetResult(HashAlgorithms.Sha256));
        using var v = all.StartVerify(jumpSectors: 8, readBack: true);
        Assert.Equal(EngineTaskState.Completed, await v.WaitAsync());
        Assert.True(v.GetReport().AllTrustworthy);
    }
}
