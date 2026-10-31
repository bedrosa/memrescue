using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>Tipo de nodo del árbol virtual (<c>MR_VFS_KIND_*</c>).</summary>
public enum VfsNodeKind : uint
{
    Unknown = 0,
    File = 1,
    Directory = 2,
    /// <summary>Carpeta propia de MemRescue (<c>[Metadatos]</c>, <c>[Huérfanos]</c>...): no existe en el origen.</summary>
    Synthetic = 3,
}

/// <summary>Categoría de una entrada (<c>MR_VFS_CAT_*</c>).</summary>
public enum VfsCategory : uint
{
    Unknown = 0,
    Live = 1,
    Deleted = 2,
    Orphan = 3,
    Metadata = 4,
    /// <summary>Reservada (M6).</summary>
    Found = 5,
    /// <summary>Reservada (M7).</summary>
    Raw = 6,
}

/// <summary>De dónde sale el nombre (<c>MR_VFS_NAME_*</c>).</summary>
public enum VfsNameReliability : uint
{
    Unknown = 0,
    /// <summary>Índice vivo o <c>$FILE_NAME</c> de un registro en uso.</summary>
    Structure = 1,
    /// <summary>Entrada de índice residual.</summary>
    Residual = 2,
    /// <summary>Solo el <c>$FILE_NAME</c> de un registro no en uso.</summary>
    Record = 3,
    Synthetic = 4,
}

/// <summary>Estado de los datos (<c>MR_VFS_DATA_*</c>). Solo <see cref="Integro"/> es íntegro.</summary>
public enum VfsDataState : uint
{
    /// <summary>Directorios y carpetas sintéticas.</summary>
    None = 0,
    Integro = 1,
    /// <summary>Borrado con sus clústeres libres: sin errores conocidos, procedencia no verificable.</summary>
    IntegroNoVerificable = 2,
    Parcial = 3,
    Sobrescrito = 4,
    SinDatos = 5,
    NoDisponible = 6,
    Cifrado = 7,
}

/// <summary>Indicadores de un nodo (<c>MR_VFS_NODE_*</c>).</summary>
[Flags]
public enum VfsNodeFlags : uint
{
    None = 0,
    Deleted = 0x0001,
    System = 0x0002,
    /// <summary>El nombre tiene unidades UTF-16 sueltas (ver <see cref="VfsNode.Name"/>, sin pérdida).</summary>
    NameLossy = 0x0004,
    TimesFromIndex = 0x0008,
    FromIndex = 0x0010,
    FromMft = 0x0020,
    RecordDamaged = 0x0040,
    ParentReused = 0x0080,
    CycleCut = 0x0100,
    DuplicateName = 0x0200,
    Anomalies = 0x0400,
    AssessmentTruncated = 0x0800,
    DirectoryIncomplete = 0x1000,
    HardLink = 0x2000,
}

/// <summary>Qué fecha usa el filtro (<c>MR_VFS_TIME_*</c>).</summary>
public enum VfsTimeField : uint
{
    Modified = 0,
    Created = 1,
    Changed = 2,
    Accessed = 3,
}

public static class VfsLabels
{
    /// <summary>Código estable de un estado (como en la CLI y el JSON).</summary>
    public static string Code(this VfsDataState s) => s switch
    {
        VfsDataState.Integro => "integro",
        VfsDataState.IntegroNoVerificable => "integro_no_verificable",
        VfsDataState.Parcial => "parcial",
        VfsDataState.Sobrescrito => "sobrescrito",
        VfsDataState.SinDatos => "sin_datos",
        VfsDataState.NoDisponible => "no_disponible",
        VfsDataState.Cifrado => "cifrado",
        _ => "",
    };

    /// <summary>Código estable de una categoría.</summary>
    public static string Code(this VfsCategory c) => c switch
    {
        VfsCategory.Live => "vivo",
        VfsCategory.Deleted => "borrado",
        VfsCategory.Orphan => "huerfano",
        VfsCategory.Metadata => "metadatos",
        VfsCategory.Found => "encontrado",
        VfsCategory.Raw => "raw",
        _ => "",
    };
}

/// <summary>Nodo del árbol virtual. <see cref="Name"/> son las unidades UTF-16 originales (sin pérdida).</summary>
public sealed record VfsNode(
    uint Id, uint? Parent, uint ChildCount, VfsNodeKind Kind, VfsCategory Category,
    VfsNameReliability NameReliability, VfsDataState DataState, VfsNodeFlags Flags, uint? NameSpace,
    uint Depth, uint Attributes, ulong Size, ulong AllocatedSize, DateTime? Created, DateTime? Modified,
    DateTime? Changed, DateTime? Accessed, ulong? Record, uint Sequence, ulong ParentRecord, uint ParentSequence,
    string Name)
{
    public bool IsContainer => Kind is VfsNodeKind.Directory or VfsNodeKind.Synthetic;

    public bool IsDeleted => Flags.HasFlag(VfsNodeFlags.Deleted);

    /// <summary><c>true</c> solo si los datos se presentan como íntegros.</summary>
    public bool IsIntact => DataState == VfsDataState.Integro;
}

/// <summary>Resumen del árbol.</summary>
public sealed record VfsInfo(
    ulong NodeCount, ulong Files, ulong Directories, ulong Synthetic, ulong Deleted, bool Partial,
    IReadOnlyDictionary<VfsCategory, ulong> ByCategory, IReadOnlyDictionary<VfsDataState, ulong> ByDataState,
    ulong MemoryBytes, uint NoteCount);

/// <summary>Filtro de una consulta (todo pasa por defecto).</summary>
public sealed record VfsFilter
{
    public bool Recursive { get; init; } = true;
    public bool DeletedOnly { get; init; }
    public bool FilesOnly { get; init; }
    public IReadOnlyCollection<VfsCategory>? Categories { get; init; }
    public IReadOnlyCollection<VfsDataState>? DataStates { get; init; }
    /// <summary>Reglas de nombre <c>regla;regla;!exclusión</c> (gana la primera que coincide).</summary>
    public string Masks { get; init; } = "";
    public ulong? SizeMin { get; init; }
    public ulong? SizeMax { get; init; }
    public VfsTimeField TimeField { get; init; } = VfsTimeField.Modified;
    public DateTime? TimeMin { get; init; }
    public DateTime? TimeMax { get; init; }
}

/// <summary>
/// Árbol virtual de un volumen: vivos, borrados, huérfanos y metadatos con el estado de sus datos
/// (inmutable; sobrevive al volumen y al origen).
/// </summary>
public sealed class VirtualFileSystem : EngineHandle
{
    internal VirtualFileSystem(ulong handle) : base(handle)
    {
        var i = new MrVfsInfo { StructSize = (uint)Marshal.SizeOf<MrVfsInfo>() };
        Engine.Check(NativeMethods.mr_vfs_info(Handle, ref i));
        Info = new VfsInfo(i.NodeCount, i.Files, i.Directories, i.Synthetic, i.Deleted, (i.Flags & 1) != 0,
            new Dictionary<VfsCategory, ulong>
            {
                [VfsCategory.Live] = i.Live,
                [VfsCategory.Deleted] = i.DeletedCategory,
                [VfsCategory.Orphan] = i.Orphan,
                [VfsCategory.Metadata] = i.Metadata,
                [VfsCategory.Found] = i.Found,
                [VfsCategory.Raw] = i.Raw,
            },
            new Dictionary<VfsDataState, ulong>
            {
                [VfsDataState.Integro] = i.Integro,
                [VfsDataState.IntegroNoVerificable] = i.IntegroNoVerificable,
                [VfsDataState.Parcial] = i.Parcial,
                [VfsDataState.Sobrescrito] = i.Sobrescrito,
                [VfsDataState.SinDatos] = i.SinDatos,
                [VfsDataState.NoDisponible] = i.NoDisponible,
                [VfsDataState.Cifrado] = i.Cifrado,
            },
            i.MemoryBytes, i.NoteCount);
    }

    /// <summary>Identificador de la raíz.</summary>
    public const uint Root = 0;

    public VfsInfo Info { get; }

    /// <summary>Nombre original del nodo (unidades UTF-16 tal como están en el origen).</summary>
    public string NameOf(uint id)
    {
        var rc = NativeMethods.mr_vfs_node_name_utf16(Handle, id, null, 0, out var len);
        if (rc != (int)EngineStatus.BufferTooSmall)
        {
            Engine.Check(rc);
        }
        if (len == 0)
        {
            return "";
        }
        var units = new ushort[(int)len];
        Engine.Check(NativeMethods.mr_vfs_node_name_utf16(Handle, id, units, len, out _));
        var chars = new char[units.Length];
        for (var k = 0; k < units.Length; k++)
        {
            chars[k] = (char)units[k];
        }
        return new string(chars);
    }

    /// <summary>Nombre visible (UTF-8 del motor, U+FFFD en las unidades sueltas).</summary>
    public string DisplayNameOf(uint id) =>
        Engine.ReadString((b, c) => (NativeMethods.mr_vfs_node_name(Handle, id, b, c, out var n), n));

    /// <summary>Ruta visible con los componentes escapados.</summary>
    public string PathOf(uint id) =>
        Engine.ReadString((b, c) => (NativeMethods.mr_vfs_node_path(Handle, id, b, c, out var n), n));

    public VfsNode GetNode(uint id)
    {
        var n = new MrVfsNode { StructSize = (uint)Marshal.SizeOf<MrVfsNode>() };
        Engine.Check(NativeMethods.mr_vfs_node(Handle, id, ref n));
        return new VfsNode(n.Id, n.Parent == uint.MaxValue ? null : n.Parent, n.ChildCount, (VfsNodeKind)n.Kind,
            (VfsCategory)n.Category, (VfsNameReliability)n.NameReliability, (VfsDataState)n.DataState,
            (VfsNodeFlags)n.Flags, n.NameSpace == 255 ? null : n.NameSpace, n.Depth, n.Attributes, n.Size,
            n.AllocatedSize, NtfsLabels.FromFileTime(n.Created), NtfsLabels.FromFileTime(n.Modified),
            NtfsLabels.FromFileTime(n.Changed), NtfsLabels.FromFileTime(n.Accessed),
            n.HasOrigin != 0 ? n.Record : null, n.Sequence, n.ParentRecord, n.ParentSequence, NameOf(id));
    }

    /// <summary>Hijos en el orden determinista del árbol.</summary>
    public IReadOnlyList<VfsNode> GetChildren(uint id = Root)
    {
        var parent = GetNode(id);
        var list = new List<VfsNode>((int)parent.ChildCount);
        for (uint k = 0; k < parent.ChildCount; k++)
        {
            Engine.Check(NativeMethods.mr_vfs_child(Handle, id, k, out var c));
            list.Add(GetNode(c));
        }
        return list;
    }

    /// <summary>Nodo de una ruta visible, o <c>null</c> si no existe.</summary>
    public VfsNode? Find(string path)
    {
        var p = Engine.Utf8(path);
        var rc = NativeMethods.mr_vfs_find(Handle, p, (nuint)p.Length, out var id);
        if (rc == (int)EngineStatus.NotFound)
        {
            return null;
        }
        Engine.Check(rc);
        return GetNode(id);
    }

    /// <summary>Notas de la construcción (anomalías del origen, límites).</summary>
    public IReadOnlyList<NtfsAnomaly> Notes()
    {
        var list = new List<NtfsAnomaly>((int)Info.NoteCount);
        for (uint k = 0; k < Info.NoteCount; k++)
        {
            var index = k;
            var t = Engine.ReadString((b, c) => (NativeMethods.mr_vfs_note_text(Handle, index, b, c, out var n), n))
                .Split('\n', 3);
            list.Add(new NtfsAnomaly(t[0], t.Length > 2 ? t[2] : ""));
        }
        return list;
    }

    private static ulong FileTime(DateTime? d, ulong none) =>
        d is { } v ? (ulong)v.ToUniversalTime().ToFileTimeUtc() : none;

    /// <summary>Nodos visibles bajo <paramref name="start"/> con el filtro, en orden determinista.</summary>
    public IReadOnlyList<uint> Query(VfsFilter filter, uint start = Root)
    {
        uint flags = 0;
        if (filter.Recursive)
        {
            flags |= 1;
        }
        if (filter.DeletedOnly)
        {
            flags |= 2;
        }
        if (filter.FilesOnly)
        {
            flags |= 4;
        }
        var f = new MrVfsFilter
        {
            StructSize = (uint)Marshal.SizeOf<MrVfsFilter>(),
            Flags = flags,
            Categories = filter.Categories?.Aggregate(0u, (m, c) => m | (1u << (int)c)) ?? 0,
            DataStates = filter.DataStates?.Aggregate(0u, (m, d) => m | (1u << (int)d)) ?? 0,
            SizeMin = filter.SizeMin ?? 0,
            SizeMax = filter.SizeMax ?? ulong.MaxValue,
            TimeMin = FileTime(filter.TimeMin, 0),
            TimeMax = FileTime(filter.TimeMax, ulong.MaxValue),
            TimeField = (uint)filter.TimeField,
        };
        var m = Engine.Utf8(filter.Masks);
        Engine.Check(NativeMethods.mr_vfs_query(Handle, start, ref f, m, (nuint)m.Length, out var q));
        try
        {
            Engine.Check(NativeMethods.mr_vfs_query_count(q, out var count));
            var list = new List<uint>((int)Math.Min(count, 10_000_000));
            for (ulong k = 0; k < count; k++)
            {
                Engine.Check(NativeMethods.mr_vfs_query_get(q, k, out var id));
                list.Add(id);
            }
            return list;
        }
        finally
        {
            NativeMethods.mr_vfs_query_free(q);
        }
    }

    private protected override void Release(ulong handle)
    {
        if (handle != 0)
        {
            NativeMethods.mr_vfs_free(handle);
        }
    }
}

/// <summary>Opciones de la construcción del árbol virtual.</summary>
public sealed record VfsBuildOptions
{
    public bool Deleted { get; init; } = true;
    public bool Orphans { get; init; } = true;
    public bool Metadata { get; init; } = true;
    /// <summary>0 = por defecto.</summary>
    public uint MaxNodes { get; init; }
}

/// <summary>Construcción del árbol virtual en segundo plano. Progreso en milésimas por fase (3000 en total).</summary>
public sealed class VfsBuildTask : EngineTask
{
    internal VfsBuildTask(ulong handle) : base(handle)
    {
    }

    public VirtualFileSystem GetResult()
    {
        Engine.Check(NativeMethods.mr_vfs_build_result(Handle, out var h));
        return new VirtualFileSystem(h);
    }
}

public static class VfsVolumeExtensions
{
    /// <summary>Lanza la construcción del árbol virtual del volumen.</summary>
    public static VfsBuildTask StartVirtualFileSystem(this NtfsVolume volume, VfsBuildOptions? options = null)
    {
        var o = options ?? new VfsBuildOptions();
        var opts = new MrVfsBuildOptions
        {
            StructSize = (uint)Marshal.SizeOf<MrVfsBuildOptions>(),
            Flags = (o.Deleted ? 0u : 1u) | (o.Orphans ? 0u : 2u) | (o.Metadata ? 0u : 4u),
            MaxNodes = o.MaxNodes,
        };
        Engine.Check(NativeMethods.mr_vfs_build_start(volume.RawHandle, ref opts, out var h));
        return new VfsBuildTask(h);
    }
}

internal static partial class NativeMethods
{
    [LibraryImport(Dll)]
    internal static partial int mr_vfs_build_start(ulong volume, ref MrVfsBuildOptions options, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_build_result(ulong task, out ulong vfs);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_info(ulong vfs, ref MrVfsInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_node(ulong vfs, uint node, ref MrVfsNode info);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_child(ulong vfs, uint node, uint index, out uint child);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_node_name(ulong vfs, uint node, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_node_name_utf16(ulong vfs, uint node, ushort[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_node_path(ulong vfs, uint node, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_find(ulong vfs, byte[] path, nuint pathLen, out uint node);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_note_text(ulong vfs, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_query(ulong vfs, uint start, ref MrVfsFilter filter, byte[] masks, nuint masksLen,
        out ulong query);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_query_count(ulong query, out ulong count);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_query_get(ulong query, ulong index, out uint node);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_query_free(ulong query);

    [LibraryImport(Dll)]
    internal static partial int mr_vfs_free(ulong vfs);
}

/// <summary>Espejo de <c>mr_vfs_build_options</c> (32 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVfsBuildOptions
{
    public uint StructSize;
    public uint Flags;
    public uint MaxNodes;
    public uint MaxDepth;
    public ulong MaxNameUnits;
    public ulong Reserved;
}

/// <summary>Espejo de <c>mr_vfs_info</c> (176 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVfsInfo
{
    public uint StructSize;
    public uint Flags;
    public ulong NodeCount;
    public ulong Files;
    public ulong Directories;
    public ulong Synthetic;
    public ulong Deleted;
    public ulong Live;
    public ulong DeletedCategory;
    public ulong Orphan;
    public ulong Metadata;
    public ulong Found;
    public ulong Raw;
    public ulong Integro;
    public ulong IntegroNoVerificable;
    public ulong Parcial;
    public ulong Sobrescrito;
    public ulong SinDatos;
    public ulong NoDisponible;
    public ulong Cifrado;
    public ulong MemoryBytes;
    public ulong NameUnits;
    public uint NoteCount;
    public uint Reserved;
}

/// <summary>Espejo de <c>mr_vfs_node</c> (128 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVfsNode
{
    public uint StructSize;
    public uint Id;
    public uint Parent;
    public uint ChildCount;
    public uint Kind;
    public uint Category;
    public uint NameReliability;
    public uint DataState;
    public uint Flags;
    public uint NameSpace;
    public uint Depth;
    public uint Attributes;
    public ulong Size;
    public ulong AllocatedSize;
    public ulong Created;
    public ulong Modified;
    public ulong Changed;
    public ulong Accessed;
    public ulong Record;
    public ulong ParentRecord;
    public uint Sequence;
    public uint ParentSequence;
    public uint HasOrigin;
    public uint Reserved;
}

/// <summary>Espejo de <c>mr_vfs_filter</c> (56 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrVfsFilter
{
    public uint StructSize;
    public uint Flags;
    public uint Categories;
    public uint DataStates;
    public ulong SizeMin;
    public ulong SizeMax;
    public ulong TimeMin;
    public ulong TimeMax;
    public uint TimeField;
    public uint Reserved;
}
