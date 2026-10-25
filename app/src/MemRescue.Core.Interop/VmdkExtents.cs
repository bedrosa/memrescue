using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>Acceso de una extensión VMDK (<c>MR_VMDK_ACCESS_*</c>).</summary>
public enum VmdkExtentAccess : uint
{
    Unknown = 0,
    ReadWrite = 1,
    ReadOnly = 2,
    /// <summary>No se abre: se lee como ilegible.</summary>
    NoAccess = 3,
}

/// <summary>Tipo de extensión VMDK (<c>MR_VMDK_EXTENT_*</c>).</summary>
public enum VmdkExtentKind : uint
{
    Unknown = 0,
    Sparse = 1,
    Flat = 2,
    Zero = 3,
    Vmfs = 4,
    VmfsSparse = 5,
    VmfsRdm = 6,
    VmfsRaw = 7,
    SeSparse = 8,
}

/// <summary>Clase de problema que impide abrir el disco (<c>MR_VMDK_ISSUE_*</c>).</summary>
public enum VmdkIssueKind : uint
{
    None = 0,
    Corrupt = 1,
    Unsupported = 2,
    /// <summary>Disco con padre: no se admite.</summary>
    Parent = 3,
}

/// <summary>Una extensión declarada en el descriptor.</summary>
/// <param name="Index">Posición en el descriptor (desde 0).</param>
/// <param name="Access">Acceso declarado.</param>
/// <param name="Kind">Tipo declarado.</param>
/// <param name="Sectors">Tamaño en sectores de 512 bytes.</param>
/// <param name="Offset">Desplazamiento en sectores dentro del fichero (FLAT).</param>
/// <param name="StartSector">Primer sector virtual que cubre.</param>
/// <param name="FileName">Nombre exactamente como lo declara el descriptor (vacío en ZERO).</param>
/// <param name="NeedsFile">Hay que aprobar un fichero para esta extensión.</param>
/// <param name="IssueKind">Clase del problema de la extensión.</param>
/// <param name="Issue">Problema de la extensión (vacío si no hay ninguno).</param>
public sealed record VmdkExtent(int Index, VmdkExtentAccess Access, VmdkExtentKind Kind, ulong Sectors,
    ulong Offset, ulong StartSector, string FileName, bool NeedsFile, VmdkIssueKind IssueKind, string Issue)
{
    public string AccessName => Access switch
    {
        VmdkExtentAccess.ReadWrite => "RW",
        VmdkExtentAccess.ReadOnly => "RDONLY",
        VmdkExtentAccess.NoAccess => "NOACCESS",
        _ => "?",
    };

    public string KindName => Kind.ToString().ToUpperInvariant();
}

/// <summary>
/// Extensiones de un descriptor VMDK separado, leídas sin abrir ninguna (incidencia 11). Para
/// abrir el disco, el usuario debe aprobar cada fichero de <see cref="RequiredNames"/> y llamar a
/// <see cref="Engine.OpenVmdk"/>.
/// </summary>
public sealed class VmdkExtentList
{
    internal VmdkExtentList(string summary, ulong virtualSize, bool hasParent, bool openable,
        VmdkIssueKind issueKind, string issue, IReadOnlyList<VmdkExtent> extents)
    {
        Summary = summary;
        VirtualSize = virtualSize;
        HasParent = hasParent;
        Openable = openable;
        IssueKind = issueKind;
        Issue = issue;
        Extents = extents;
    }

    public string Summary { get; }

    public ulong VirtualSize { get; }

    public bool HasParent { get; }

    /// <summary>El disco se puede abrir una vez aprobados sus ficheros.</summary>
    public bool Openable { get; }

    public VmdkIssueKind IssueKind { get; }

    /// <summary>Primer problema que impide abrir el disco (vacío si no hay ninguno).</summary>
    public string Issue { get; }

    public IReadOnlyList<VmdkExtent> Extents { get; }

    /// <summary>Nombres declarados para los que hay que aprobar un fichero, en orden.</summary>
    public IEnumerable<string> RequiredNames => Extents.Where(e => e.NeedsFile).Select(e => e.FileName);
}

/// <summary>Fichero aprobado por el usuario para un nombre declarado del descriptor.</summary>
public sealed record ApprovedExtent(string Name, string Path);

/// <summary>Espejo de <c>mr_vmdk_extents_info</c> (32 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVmdkExtentsInfo
{
    public uint StructSize;
    public uint ExtentCount;
    public uint RequiredCount;
    public uint Flags;
    public ulong VirtualSize;
    public uint IssueKind;
    public uint Reserved;
}

/// <summary>Espejo de <c>mr_vmdk_extent</c> (48 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVmdkExtent
{
    public uint StructSize;
    public uint Access;
    public uint Kind;
    public uint Flags;
    public ulong Sectors;
    public ulong Offset;
    public ulong StartSector;
    public uint IssueKind;
    public uint Reserved;
}

/// <summary>
/// Espejo de <c>mr_approved_file</c> (40 bytes en 64 bits). <see cref="Name"/> y <see cref="Path"/>
/// apuntan a UTF-8 fijado en memoria durante la llamada.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrApprovedFile
{
    public uint StructSize;
    public uint Reserved;
    public nint Name;
    public nuint NameLen;
    public nint Path;
    public nuint PathLen;
}

public static partial class Engine
{
    /// <summary>Lista las extensiones de un descriptor VMDK separado sin abrir ninguna.</summary>
    public static VmdkExtentList ListVmdkExtents(string descriptorPath)
    {
        var p = Utf8(descriptorPath);
        Check(NativeMethods.mr_vmdk_extents_list(p, (nuint)p.Length, out var h));
        try
        {
            var info = new MrVmdkExtentsInfo { StructSize = (uint)Marshal.SizeOf<MrVmdkExtentsInfo>() };
            Check(NativeMethods.mr_vmdk_extents_info(h, ref info));
            var summary = ReadString((b, c) => (NativeMethods.mr_vmdk_extents_describe(h, b, c, out var n), n));
            var issue = ReadString((b, c) => (NativeMethods.mr_vmdk_extents_issue(h, b, c, out var n), n));
            var list = new List<VmdkExtent>((int)Math.Min(info.ExtentCount, 4096u));
            for (uint i = 0; i < info.ExtentCount; i++)
            {
                var e = new MrVmdkExtent { StructSize = (uint)Marshal.SizeOf<MrVmdkExtent>() };
                Check(NativeMethods.mr_vmdk_extent_get(h, i, ref e));
                var index = i;
                var name = ReadString((b, c) => (NativeMethods.mr_vmdk_extent_name(h, index, b, c, out var n), n));
                var eIssue = ReadString((b, c) => (NativeMethods.mr_vmdk_extent_issue(h, index, b, c, out var n), n));
                list.Add(new VmdkExtent((int)i, (VmdkExtentAccess)e.Access, (VmdkExtentKind)e.Kind, e.Sectors,
                    e.Offset, e.StartSector, name, (e.Flags & NativeMethods.VmdkExtentNeedsFile) != 0,
                    (VmdkIssueKind)e.IssueKind, eIssue));
            }
            return new VmdkExtentList(summary, info.VirtualSize, (info.Flags & NativeMethods.VmdkHasParent) != 0,
                (info.Flags & NativeMethods.VmdkOpenable) != 0, (VmdkIssueKind)info.IssueKind, issue, list);
        }
        finally
        {
            NativeMethods.mr_vmdk_extents_free(h);
        }
    }

    /// <summary>
    /// Abre un VMDK con descriptor separado usando SOLO los ficheros aprobados explícitamente por el
    /// usuario (nombre declarado, coincidencia exacta → fichero elegido). El motor no abre ningún
    /// otro fichero; una extensión necesaria sin aprobar da <see cref="EngineStatus.ExternalReference"/>.
    /// </summary>
    public static Source OpenVmdk(string descriptorPath, IReadOnlyList<ApprovedExtent> files, uint sectorSize = 0,
        bool tolerant = false)
    {
        ArgumentNullException.ThrowIfNull(files);
        var p = Utf8(descriptorPath);
        var handles = new List<GCHandle>(files.Count * 2);
        try
        {
            var items = new MrApprovedFile[files.Count];
            for (var i = 0; i < files.Count; i++)
            {
                var name = Utf8(files[i].Name);
                var path = Utf8(files[i].Path);
                var hn = GCHandle.Alloc(name, GCHandleType.Pinned);
                handles.Add(hn);
                var hp = GCHandle.Alloc(path, GCHandleType.Pinned);
                handles.Add(hp);
                items[i] = new MrApprovedFile
                {
                    StructSize = (uint)Marshal.SizeOf<MrApprovedFile>(),
                    Name = hn.AddrOfPinnedObject(),
                    NameLen = (nuint)name.Length,
                    Path = hp.AddrOfPinnedObject(),
                    PathLen = (nuint)path.Length,
                };
            }
            Check(NativeMethods.mr_source_open_vmdk(p, (nuint)p.Length, sectorSize,
                tolerant ? NativeMethods.OpenTolerant : 0, items, (uint)items.Length, out var h));
            return new Source(h, descriptorPath);
        }
        finally
        {
            foreach (var h in handles)
            {
                h.Free();
            }
        }
    }
}
