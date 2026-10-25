using System.Runtime.InteropServices;
using System.Security.Cryptography;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>
/// VMDK con descriptor separado (incidencia 11): el descriptor se lista sin abrir nada y las
/// extensiones solo se abren cuando se aprueban una a una. Ficheros sintéticos en un directorio
/// temporal; se comprueba que no cambian.
/// </summary>
public sealed class VmdkExtentTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-vmdk-ext-").FullName;

    public void Dispose()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        Directory.Delete(_dir, recursive: true);
    }

    private string Write(string name, byte[] content)
    {
        var p = Path.Combine(_dir, name);
        File.WriteAllBytes(p, content);
        return p;
    }

    private static byte[] Filled(int sectors, byte seed) =>
        Enumerable.Range(0, sectors * 512).Select(i => (byte)(i * 7 + seed)).ToArray();

    [Fact]
    public void StructLayoutsMatchHeader()
    {
        Assert.Equal(32, Marshal.SizeOf<MrVmdkExtentsInfo>());
        Assert.Equal(48, Marshal.SizeOf<MrVmdkExtent>());
        Assert.Equal(40, Marshal.SizeOf<MrApprovedFile>());
    }

    [Fact]
    public void ExtentsAreListedAndOpenedOnlyWithExplicitApproval()
    {
        var a = Filled(8, 1);
        var b = Filled(8, 2);
        var pa = Write("v-f001.vmdk", a);
        var pb = Write("v-f002.vmdk", b);
        var desc = Write("v.vmdk", System.Text.Encoding.UTF8.GetBytes(
            "# Disk DescriptorFile\nCID=1\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\n" +
            "RW 8 FLAT \"v-f001.vmdk\" 0\nRW 8 FLAT \"v-f002.vmdk\" 0\nRW 8 ZERO\nNOACCESS 8 FLAT \"..\\\\nada\" 0\n"));
        var hashes = new[] { pa, pb, desc }.Select(p => Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(p)))).ToArray();

        Assert.Equal(ImageFormat.VmdkDescriptor, Engine.Probe(desc));
        var list = Engine.ListVmdkExtents(desc);
        Assert.True(list.Openable);
        Assert.False(list.HasParent);
        Assert.Equal(32UL * 512, list.VirtualSize);
        Assert.Equal(["v-f001.vmdk", "v-f002.vmdk"], list.RequiredNames.ToArray());
        Assert.Equal(VmdkExtentKind.Zero, list.Extents[2].Kind);
        Assert.Equal("", list.Extents[2].FileName);
        Assert.Equal(VmdkExtentAccess.NoAccess, list.Extents[3].Access);
        Assert.False(list.Extents[3].NeedsFile);
        Assert.Contains("twoGbMaxExtentFlat", list.Summary);

        // La apertura normal no sigue la referencia; sin aprobar (o con una sola) tampoco se abre.
        Assert.Equal(EngineStatus.ExternalReference, Assert.Throws<EngineException>(() => Engine.OpenSource(desc)).Status);
        Assert.Equal(EngineStatus.ExternalReference, Assert.Throws<EngineException>(() => Engine.OpenVmdk(desc, [])).Status);
        Assert.Equal(EngineStatus.ExternalReference,
            Assert.Throws<EngineException>(() => Engine.OpenVmdk(desc, [new ApprovedExtent("v-f001.vmdk", pa)])).Status);
        // Nombre que no coincide exactamente con el descriptor.
        Assert.Equal(EngineStatus.InvalidArgument, Assert.Throws<EngineException>(() =>
            Engine.OpenVmdk(desc, [new ApprovedExtent("V-F001.vmdk", pa), new ApprovedExtent("v-f002.vmdk", pb)])).Status);
        Assert.Equal(EngineStatus.PathRejected, Assert.Throws<EngineException>(() =>
            Engine.OpenVmdk(desc, [new ApprovedExtent("v-f001.vmdk", @"\\.\PhysicalDrive0"), new ApprovedExtent("v-f002.vmdk", pb)])).Status);

        using (var src = Engine.OpenVmdk(desc, [new ApprovedExtent("v-f001.vmdk", pa), new ApprovedExtent("v-f002.vmdk", pb)]))
        {
            Assert.Equal(32UL * 512, src.Info.SizeBytes);
            Assert.Contains("FLAT", src.Description);
            Assert.Equal([.. a, .. b], src.ReadSectors(0, 16));
            Assert.All(src.ReadSectors(16, 8), x => Assert.Equal(0, x));
            // NOACCESS: la lectura con estado lo marca ilegible.
            var (_, states) = src.ReadSectorsMapped(24, 8);
            Assert.All(states, s => Assert.Equal(SectorState.Unreadable, s));
        }

        Assert.Equal(hashes, new[] { pa, pb, desc }.Select(p => Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(p)))).ToArray());
        Assert.False(File.Exists(Path.Combine(_dir, "..", "nada")));
    }

    [Fact]
    public void UnsafeNamesInTheDescriptorAreReported()
    {
        var p = Write("x-flat.vmdk", Filled(8, 3));
        var desc = Write("u.vmdk", System.Text.Encoding.UTF8.GetBytes(
            "# Disk DescriptorFile\ncreateType=\"monolithicFlat\"\nRW 8 FLAT \"C:\\\\Windows\\\\x-flat.vmdk\" 0\n"));
        var list = Engine.ListVmdkExtents(desc);
        Assert.False(list.Openable);
        Assert.Equal(VmdkIssueKind.Unsupported, list.IssueKind);
        Assert.NotEmpty(list.Extents[0].Issue);
        Assert.Equal(EngineStatus.UnsupportedFeature, Assert.Throws<EngineException>(() =>
            Engine.OpenVmdk(desc, [new ApprovedExtent(list.Extents[0].FileName, p)])).Status);
    }
}
