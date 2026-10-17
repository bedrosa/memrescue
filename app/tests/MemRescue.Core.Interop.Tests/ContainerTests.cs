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

    /// <summary>Suma de control del VHD: complemento a uno de la suma de los bytes, sin el propio campo.</summary>
    private static void SealVhd(byte[] b, int field)
    {
        uint sum = 0;
        for (var i = 0; i < b.Length; i++)
        {
            if (i < field || i >= field + 4)
            {
                sum += b[i];
            }
        }
        BinaryPrimitives.WriteUInt32BigEndian(b.AsSpan(field), ~sum);
    }

    /// <summary>
    /// VHD dinámico según la especificación pública: 4 bloques de 4 KiB (0, 1 y 2 asignados) con la
    /// entrada de la BAT del bloque 1 fuera del fichero. Cada byte del bloque b vale 0x10 + b.
    /// </summary>
    internal static byte[] DamagedDynamicVhd()
    {
        const int bs = 4096;
        var footer = new byte[512];
        "conectix"u8.CopyTo(footer);
        BinaryPrimitives.WriteUInt32BigEndian(footer.AsSpan(8), 2);
        BinaryPrimitives.WriteUInt32BigEndian(footer.AsSpan(12), 0x0001_0000);
        BinaryPrimitives.WriteUInt64BigEndian(footer.AsSpan(16), 512);
        BinaryPrimitives.WriteUInt64BigEndian(footer.AsSpan(40), 4 * bs);
        BinaryPrimitives.WriteUInt64BigEndian(footer.AsSpan(48), 4 * bs);
        BinaryPrimitives.WriteUInt32BigEndian(footer.AsSpan(60), 3);
        SealVhd(footer, 64);
        var header = new byte[1024];
        "cxsparse"u8.CopyTo(header);
        BinaryPrimitives.WriteUInt64BigEndian(header.AsSpan(8), ulong.MaxValue);
        BinaryPrimitives.WriteUInt64BigEndian(header.AsSpan(16), 1536);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(24), 0x0001_0000);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(28), 4);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(32), bs);
        SealVhd(header, 36);
        var bat = Enumerable.Repeat((byte)0xFF, 512).ToArray();
        var body = new List<byte>();
        for (var b = 0; b < 3; b++)
        {
            var sector = (uint)((2048 + body.Count) / 512);
            BinaryPrimitives.WriteUInt32BigEndian(bat.AsSpan(b * 4), b == 1 ? 0x00FF_0000u : sector);
            var bitmap = new byte[512];
            bitmap[0] = 0xFF;
            body.AddRange(bitmap);
            body.AddRange(Enumerable.Repeat((byte)(0x10 + b), bs));
        }
        return [.. footer, .. header, .. bat, .. body, .. footer];
    }

    [Fact]
    public async Task TolerantModeIsExplicitAndDiscardedBlocksAreUntrusted()
    {
        var p = Write("t.vhd", DamagedDynamicVhd());
        var before = SHA256.HashData(File.ReadAllBytes(p));
        var modified = File.GetLastWriteTimeUtc(p);
        // Por defecto, estricto: dañado.
        Assert.Equal(EngineStatus.Corrupt, Assert.Throws<EngineException>(() => Engine.OpenSource(p)).Status);
        using (var src = Engine.OpenSource(p, tolerant: true))
        {
            Assert.Contains("modo tolerante: 1 bloque descartado", src.Description);
            var m = src.ReadSectorsMapped(0, 32, retries: 0);
            for (var l = 0; l < 32; l++)
            {
                var sector = m.Data.AsSpan(l * 512, 512).ToArray();
                if (l is >= 8 and < 16)
                {
                    Assert.Equal(SectorState.Unreadable, m.States[l]);
                    Assert.Equal("BAD!"u8.ToArray(), sector[..4]);
                }
                else
                {
                    Assert.True(m.States[l].IsTrustworthy());
                    Assert.All(sector, b => Assert.Equal(l >= 24 ? 0 : 0x10 + l / 8, b));
                }
            }
            // Lectura estricta y hash: fallan; la verificación cuenta los 8 sectores no fiables.
            Assert.Equal(EngineStatus.Io, Assert.Throws<EngineException>(() => src.ReadSectors(8, 1)).Status);
            using var h = src.StartHash(HashAlgorithms.Sha256);
            Assert.Equal(EngineTaskState.Failed, await h.WaitAsync());
            using var v = src.StartVerify();
            Assert.Equal(EngineTaskState.Completed, await v.WaitAsync());
            var r = v.GetReport();
            Assert.Equal(8ul, r.SectorsUnreadable);
            Assert.Equal(new UntrustedRange(8, 8, SectorState.Unreadable), Assert.Single(r.UntrustedRanges));
        }
        Assert.Equal(before, SHA256.HashData(File.ReadAllBytes(p)));
        Assert.Equal(modified, File.GetLastWriteTimeUtc(p));
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
