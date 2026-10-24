using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>Estado de una entrada de directorio NTFS contrastada con la MFT (<c>MR_NTFS_ENTRY_*</c>).</summary>
public enum NtfsEntryState : uint
{
    Unknown = 0,
    /// <summary>El registro está en uso con la secuencia del índice: fichero vivo.</summary>
    Live = 1,
    /// <summary>El registro ya no está en uso: entrada residual.</summary>
    StaleNotInUse = 2,
    /// <summary>El registro se reutilizó para otro fichero (otra secuencia): entrada residual.</summary>
    StaleReused = 3,
    /// <summary>El registro no es interpretable (dañado, ilegible...).</summary>
    RecordUnusable = 4,
    /// <summary>Referencia fuera de la MFT o a un registro de extensión.</summary>
    InvalidReference = 5,
}

/// <summary>Indicadores de una entrada (<c>MR_NTFS_ENTRY_DIRECTORY</c>...).</summary>
[Flags]
public enum NtfsEntryFlags : uint
{
    None = 0,
    Directory = 1,
    /// <summary>Fichero del sistema (registros 0-15 y contenido de <c>$Extend</c>).</summary>
    System = 2,
    /// <summary>El nombre tenía unidades UTF-16 sueltas, mostradas como U+FFFD.</summary>
    NameLossy = 4,
    HasDosName = 8,
    /// <summary>Fechas de la clave del índice (el registro no se pudo usar).</summary>
    TimesFromIndex = 16,
}

/// <summary>Estado final del árbol (<c>MR_NTFS_TREE_*</c>).</summary>
public enum NtfsTreeStatus : uint
{
    Unknown = 0,
    Complete = 1,
    /// <summary>Límite de nodos alcanzado: árbol parcial.</summary>
    Limit = 2,
    /// <summary>Cancelado: árbol parcial.</summary>
    Cancelled = 3,
}

public static class NtfsLabels
{
    public static string Label(this NtfsEntryState s) => s switch
    {
        NtfsEntryState.Live => "vivo",
        NtfsEntryState.StaleNotInUse => "RESIDUAL: registro libre",
        NtfsEntryState.StaleReused => "RESIDUAL: registro reutilizado",
        NtfsEntryState.RecordUnusable => "REGISTRO NO UTILIZABLE",
        NtfsEntryState.InvalidReference => "REFERENCIA NO VÁLIDA",
        _ => "desconocido",
    };

    /// <summary>FILETIME (UTC) como fecha, o <c>null</c> si no es representable.</summary>
    public static DateTime? FromFileTime(ulong ft) =>
        ft is > 0 and < 2_650_467_743_999_999_999UL ? DateTime.FromFileTimeUtc((long)ft) : null;
}

/// <summary>Datos del volumen NTFS.</summary>
public sealed record NtfsVolumeInfo(
    string Label, ulong Serial, ulong RecordCount, ulong TotalClusters, uint ClusterSize, uint RecordSize,
    uint IndexRecordSize, uint BytesPerSector, (uint Major, uint Minor)? Version, ConfidenceLevel Confidence,
    bool BootFromBackup, bool UpcaseFromVolume, ulong MftLcn, IReadOnlyList<NtfsAnomaly> Anomalies);

/// <summary>Anomalía con su código estable.</summary>
public sealed record NtfsAnomaly(string Code, string Message)
{
    public override string ToString() => $"{Code}: {Message}";
}

/// <summary>Entrada de un directorio NTFS.</summary>
public sealed record NtfsEntry(
    string Name, ulong Record, uint Sequence, ulong ParentRecord, uint ParentSequence, ulong Size,
    ulong AllocatedSize, DateTime? Created, DateTime? Modified, DateTime? MftModified, DateTime? Accessed,
    uint FileAttributes, NtfsEntryFlags Flags, uint NameSpace, uint LinkCount, NtfsEntryState State,
    uint RecordState, uint FoundSequence)
{
    public bool IsDirectory => Flags.HasFlag(NtfsEntryFlags.Directory);

    public bool IsSystem => Flags.HasFlag(NtfsEntryFlags.System);

    public bool IsLive => State == NtfsEntryState.Live;

    internal static NtfsEntry From(string name, in MrNtfsEntry e) =>
        new(name, e.Record, e.Sequence, e.ParentRecord, e.ParentSequence, e.Size, e.AllocatedSize,
            NtfsLabels.FromFileTime(e.Created), NtfsLabels.FromFileTime(e.Modified),
            NtfsLabels.FromFileTime(e.MftModified), NtfsLabels.FromFileTime(e.Accessed), e.FileAttributes,
            (NtfsEntryFlags)e.Flags, e.NameSpace, e.LinkCount, (NtfsEntryState)e.State, e.RecordState, e.FoundSequence);

    internal static MrNtfsEntry Empty() => new() { StructSize = (uint)Marshal.SizeOf<MrNtfsEntry>() };
}

/// <summary>Listado de un directorio: entradas vivas y residuales (ver <see cref="NtfsEntry.State"/>) y anomalías del índice.</summary>
public sealed record NtfsDirectoryListing(
    ulong DirectoryRecord, uint DirectorySequence, IReadOnlyList<NtfsEntry> Entries,
    IReadOnlyList<NtfsAnomaly> Anomalies, bool Unreadable)
{
    public IEnumerable<NtfsEntry> Live => Entries.Where(e => e.IsLive);
}

/// <summary>Nodo del árbol (el 0 es la raíz, con <see cref="ParentIndex"/> = -1).</summary>
public sealed record NtfsTreeNode(string Path, long ParentIndex, uint Depth, NtfsEntry Entry);

/// <summary>Anomalía del árbol en un directorio.</summary>
public sealed record NtfsTreeAnomaly(string Code, string Path, string Message);

/// <summary>Registro en uso que no se alcanza desde la raíz.</summary>
public sealed record NtfsOrphan(ulong Record, uint Sequence, ulong ParentRecord, uint ParentSequence,
    bool IsDirectory, uint Reason, string Code, string Name, string Message);

/// <summary>Árbol completo del volumen.</summary>
public sealed record NtfsTree(NtfsTreeStatus Status, IReadOnlyList<NtfsTreeNode> Nodes,
    IReadOnlyList<NtfsOrphan> Orphans, IReadOnlyList<NtfsTreeAnomaly> Anomalies, ulong Directories);

/// <summary>Informe de una copia de fichero.</summary>
public sealed record NtfsCopyReport(ulong Bytes, ulong UntrustedBytes, ulong SparseBytes, ulong UninitializedBytes,
    uint UntrustedRangeCount, bool RangesTruncated, bool RecordNotInUse)
{
    /// <summary>Parte de <see cref="UntrustedBytes"/> en clústeres que pueden haberse reutilizado (nunca datos fiables).</summary>
    public ulong PossiblyOverwrittenBytes { get; init; }

    /// <summary><c>true</c> si todos los bytes son datos fiables del origen.</summary>
    public bool AllTrustworthy => UntrustedBytes == 0;
}

/// <summary>
/// Volumen NTFS abierto en el motor (solo lectura). Sobrevive al cierre de su origen; los listados,
/// tareas y orígenes abiertos desde él sobreviven a su liberación.
/// </summary>
public sealed class NtfsVolume : EngineHandle
{
    internal NtfsVolume(ulong handle, string sourcePath) : base(handle)
    {
        SourcePath = sourcePath;
        var i = new MrNtfsVolumeInfo { StructSize = (uint)Marshal.SizeOf<MrNtfsVolumeInfo>() };
        Engine.Check(NativeMethods.mr_ntfs_volume_info(Handle, ref i));
        var label = Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_volume_label(Handle, b, c, out var n), n));
        var anomalies = new List<NtfsAnomaly>();
        for (uint k = 0; k < i.AnomalyCount; k++)
        {
            var index = k;
            anomalies.Add(Split(Engine.ReadString((b, c) =>
                (NativeMethods.mr_ntfs_volume_anomaly_text(Handle, index, b, c, out var n), n))));
        }
        Info = new NtfsVolumeInfo(label, i.Serial, i.RecordCount, i.TotalClusters, i.ClusterSize, i.RecordSize,
            i.IndexRecordSize, i.BytesPerSector,
            (i.Flags & 2) != 0 ? (i.VersionMajor, i.VersionMinor) : null, (ConfidenceLevel)i.Confidence,
            (i.Flags & 1) != 0, (i.Flags & 4) != 0, i.MftLcn, anomalies);
    }

    /// <summary>Registro del directorio raíz.</summary>
    public const ulong RootRecord = 5;

    /// <summary>Ruta de la imagen de la que se abrió (los orígenes de ficheros la conservan).</summary>
    public string SourcePath { get; }

    public NtfsVolumeInfo Info { get; }

    internal static NtfsAnomaly Split(string text)
    {
        var p = text.Split('\n', 2);
        return new NtfsAnomaly(p[0], p.Length > 1 ? p[1] : "");
    }

    /// <summary>Lista el directorio del registro <paramref name="record"/> (5 = raíz).</summary>
    public NtfsDirectoryListing ListDirectory(ulong record = RootRecord)
    {
        Engine.Check(NativeMethods.mr_ntfs_list_dir(Handle, record, out var h));
        try
        {
            var li = new MrNtfsListingInfo { StructSize = (uint)Marshal.SizeOf<MrNtfsListingInfo>() };
            Engine.Check(NativeMethods.mr_ntfs_listing_info(h, ref li));
            var entries = new List<NtfsEntry>((int)li.EntryCount);
            for (uint i = 0; i < li.EntryCount; i++)
            {
                var e = NtfsEntry.Empty();
                Engine.Check(NativeMethods.mr_ntfs_listing_entry(h, i, ref e));
                var index = i;
                var name = Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_listing_entry_name(h, index, b, c, out var n), n));
                entries.Add(NtfsEntry.From(name, e));
            }
            var anomalies = new List<NtfsAnomaly>((int)li.AnomalyCount);
            for (uint i = 0; i < li.AnomalyCount; i++)
            {
                var index = i;
                anomalies.Add(Split(Engine.ReadString((b, c) =>
                    (NativeMethods.mr_ntfs_listing_anomaly_text(h, index, b, c, out var n), n))));
            }
            return new NtfsDirectoryListing(li.DirectoryRecord, li.DirectorySequence, entries, anomalies, li.Unreadable != 0);
        }
        finally
        {
            NativeMethods.mr_ntfs_listing_free(h);
        }
    }

    /// <summary>
    /// Resuelve una ruta absoluta (<c>/a/b</c> o <c>\a\b</c>) sin distinguir mayúsculas. Si no existe,
    /// <see cref="EngineStatus.NotFound"/>.
    /// </summary>
    public NtfsEntry Resolve(string path)
    {
        var p = Engine.Utf8(path);
        var e = NtfsEntry.Empty();
        Engine.Check(NativeMethods.mr_ntfs_resolve(Handle, p, (nuint)p.Length, ref e));
        var name = path.Replace('\\', '/').TrimEnd('/').Split('/')[^1];
        return NtfsEntry.From(name, e);
    }

    /// <summary>Ruta de un registro; si está huérfano, <see cref="EngineStatus.Corrupt"/> con el motivo.</summary>
    public string PathOf(ulong record) =>
        Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_path_of(Handle, record, b, c, out var n), n));

    /// <summary>Lanza el árbol completo (con la búsqueda de huérfanos salvo que se pida lo contrario).</summary>
    public NtfsTreeTask StartTree(bool orphans = true)
    {
        Engine.Check(NativeMethods.mr_ntfs_tree_start(Handle, orphans ? 0u : 1u, out var h));
        return new NtfsTreeTask(h);
    }

    /// <summary>
    /// Abre el flujo <c>$DATA</c> del fichero como un origen de sectores de 512 B para el visor: los
    /// tramos no fiables se leen como sectores ilegibles (patrón de relleno, nunca datos).
    /// </summary>
    public Source OpenStream(ulong record, string stream = "")
    {
        var s = Engine.Utf8(stream);
        Engine.Check(NativeMethods.mr_ntfs_open_stream(Handle, record, s, (nuint)s.Length, out var h));
        return new Source(h, SourcePath);
    }

    /// <summary>
    /// Copia el fichero a <paramref name="outputPath"/> en segundo plano. Nunca sobre el origen ni un
    /// dispositivo; si la salida existe, <see cref="EngineStatus.AlreadyExists"/> salvo con
    /// <paramref name="overwrite"/>; la escritura es atómica (temporal y renombrado final).
    /// </summary>
    public NtfsCopyTask StartCopy(ulong record, string outputPath, string stream = "", bool overwrite = false)
    {
        var s = Engine.Utf8(stream);
        var o = Engine.Utf8(outputPath);
        Engine.Check(NativeMethods.mr_ntfs_copy_start(Handle, record, s, (nuint)s.Length, o, (nuint)o.Length,
            overwrite ? 1u : 0u, out var h));
        return new NtfsCopyTask(h);
    }

    private protected override void Release(ulong handle)
    {
        if (handle != 0)
        {
            NativeMethods.mr_ntfs_volume_free(handle);
        }
    }
}

/// <summary>Árbol NTFS en segundo plano. Progreso: entradas listadas más registros revisados.</summary>
public sealed class NtfsTreeTask : EngineTask
{
    internal NtfsTreeTask(ulong handle) : base(handle)
    {
    }

    public NtfsTree GetResult()
    {
        var i = new MrNtfsTreeInfo { StructSize = (uint)Marshal.SizeOf<MrNtfsTreeInfo>() };
        Engine.Check(NativeMethods.mr_ntfs_tree_result(Handle, ref i));
        var nodes = new List<NtfsTreeNode>((int)Math.Min(i.NodeCount, 1_000_000));
        for (ulong k = 0; k < i.NodeCount; k++)
        {
            var e = NtfsEntry.Empty();
            Engine.Check(NativeMethods.mr_ntfs_tree_node(Handle, k, out var parent, out var depth, ref e));
            var index = k;
            var path = Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_tree_node_path(Handle, index, b, c, out var n), n));
            var name = k == 0 ? "" : path[(path.LastIndexOf('/') + 1)..];
            nodes.Add(new NtfsTreeNode(path, parent == ulong.MaxValue ? -1 : (long)parent, depth, NtfsEntry.From(name, e)));
        }
        var orphans = new List<NtfsOrphan>();
        for (uint k = 0; k < i.OrphanCount; k++)
        {
            var o = new MrNtfsOrphan { StructSize = (uint)Marshal.SizeOf<MrNtfsOrphan>() };
            Engine.Check(NativeMethods.mr_ntfs_tree_orphan(Handle, k, ref o));
            var index = k;
            var t = Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_tree_orphan_text(Handle, index, b, c, out var n), n))
                .Split('\n', 3);
            orphans.Add(new NtfsOrphan(o.Record, o.Sequence, o.ParentRecord, o.ParentSequence, (o.Flags & 1) != 0,
                o.Reason, t[0], t.Length > 1 ? t[1] : "", t.Length > 2 ? t[2] : ""));
        }
        var anomalies = new List<NtfsTreeAnomaly>();
        for (uint k = 0; k < i.AnomalyCount; k++)
        {
            var index = k;
            var t = Engine.ReadString((b, c) => (NativeMethods.mr_ntfs_tree_anomaly_text(Handle, index, b, c, out var n), n))
                .Split('\n', 3);
            anomalies.Add(new NtfsTreeAnomaly(t[0], t.Length > 1 ? t[1] : "", t.Length > 2 ? t[2] : ""));
        }
        return new NtfsTree((NtfsTreeStatus)i.Status, nodes, orphans, anomalies, i.Directories);
    }
}

/// <summary>Copia de un fichero NTFS en segundo plano. Progreso en bytes.</summary>
public sealed class NtfsCopyTask : EngineTask
{
    internal NtfsCopyTask(ulong handle) : base(handle)
    {
    }

    public NtfsCopyReport GetReport()
    {
        var r = new MrNtfsCopyReport { StructSize = (uint)Marshal.SizeOf<MrNtfsCopyReport>() };
        Engine.Check(NativeMethods.mr_ntfs_copy_result(Handle, ref r));
        return new NtfsCopyReport(r.Bytes, r.UntrustedBytes, r.SparseBytes, r.UninitializedBytes,
            r.UntrustedRangeCount, (r.Flags & 1) != 0, (r.Flags & 2) != 0)
        {
            PossiblyOverwrittenBytes = r.PossiblyOverwrittenBytes,
        };
    }
}

internal static partial class NativeMethods
{
    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_open(ulong source, out ulong volume);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_volume_info(ulong volume, ref MrNtfsVolumeInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_volume_label(ulong volume, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_volume_anomaly_text(ulong volume, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_volume_free(ulong volume);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_list_dir(ulong volume, ulong record, out ulong listing);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_listing_info(ulong listing, ref MrNtfsListingInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_listing_entry(ulong listing, uint index, ref MrNtfsEntry entry);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_listing_entry_name(ulong listing, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_listing_anomaly_text(ulong listing, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_listing_free(ulong listing);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_resolve(ulong volume, byte[] path, nuint pathLen, ref MrNtfsEntry entry);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_path_of(ulong volume, ulong record, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_tree_start(ulong volume, uint flags, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_tree_result(ulong task, ref MrNtfsTreeInfo info);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_tree_node(ulong task, ulong index, out ulong parent, out uint depth, ref MrNtfsEntry entry);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_tree_node_path(ulong task, ulong index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_tree_anomaly_text(ulong task, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_tree_orphan(ulong task, uint index, ref MrNtfsOrphan orphan);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_tree_orphan_text(ulong task, uint index, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_open_stream(ulong volume, ulong record, byte[] stream, nuint streamLen, out ulong source);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_copy_start(ulong volume, ulong record, byte[] stream, nuint streamLen,
        byte[] outPath, nuint outPathLen, uint flags, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_ntfs_copy_result(ulong task, ref MrNtfsCopyReport report);
}

/// <summary>Espejo de <c>mr_ntfs_volume_info</c> (72 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsVolumeInfo
{
    public uint StructSize;
    public uint Confidence;
    public ulong Serial;
    public ulong RecordCount;
    public ulong TotalClusters;
    public uint ClusterSize;
    public uint RecordSize;
    public uint IndexRecordSize;
    public uint BytesPerSector;
    public uint VersionMajor;
    public uint VersionMinor;
    public uint Flags;
    public uint AnomalyCount;
    public ulong MftLcn;
}

/// <summary>Espejo de <c>mr_ntfs_entry</c> (104 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsEntry
{
    public uint StructSize;
    public uint State;
    public ulong Record;
    public ulong ParentRecord;
    public uint Sequence;
    public uint ParentSequence;
    public ulong Size;
    public ulong AllocatedSize;
    public ulong Created;
    public ulong Modified;
    public ulong MftModified;
    public ulong Accessed;
    public uint FileAttributes;
    public uint Flags;
    public uint NameSpace;
    public uint LinkCount;
    public uint RecordState;
    public uint FoundSequence;
}

/// <summary>Espejo de <c>mr_ntfs_listing_info</c> (32 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsListingInfo
{
    public uint StructSize;
    public uint EntryCount;
    public ulong DirectoryRecord;
    public uint DirectorySequence;
    public uint AnomalyCount;
    public uint LiveCount;
    public uint Unreadable;
}

/// <summary>Espejo de <c>mr_ntfs_tree_info</c> (40 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsTreeInfo
{
    public uint StructSize;
    public uint Status;
    public ulong NodeCount;
    public ulong OrphanCount;
    public uint AnomalyCount;
    public uint Reserved;
    public ulong Directories;
}

/// <summary>Espejo de <c>mr_ntfs_orphan</c> (40 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsOrphan
{
    public uint StructSize;
    public uint Reason;
    public ulong Record;
    public ulong ParentRecord;
    public uint Sequence;
    public uint ParentSequence;
    public uint Flags;
    public uint Reserved;
}

/// <summary>Espejo de <c>mr_ntfs_copy_report</c> (56 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrNtfsCopyReport
{
    public uint StructSize;
    public uint UntrustedRangeCount;
    public ulong Bytes;
    public ulong UntrustedBytes;
    public ulong SparseBytes;
    public ulong UninitializedBytes;
    public ulong PossiblyOverwrittenBytes;
    public uint Flags;
    public uint Reserved;
}
