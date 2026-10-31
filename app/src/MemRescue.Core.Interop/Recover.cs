using System.Runtime.InteropServices;

namespace MemRescue.Core.Interop;

/// <summary>Política ante un nombre que ya existe en el destino (<c>MR_RECOVER_CONFLICT_*</c>).</summary>
public enum RecoverConflictPolicy : uint
{
    /// <summary>Pregunta por cada conflicto (<see cref="RecoverTask.PendingQuestion"/>).</summary>
    Ask = 1,
    /// <summary><c>nombre (2).ext</c>; <c>nombre (borrado).ext</c> para la versión borrada de un nombre vivo.</summary>
    Rename = 2,
    Skip = 3,
    /// <summary>Omite si el existente tiene el mismo tamaño y SHA-256 (reanudar = repetir con esta política).</summary>
    SkipIfIdentical = 4,
    /// <summary>Sustituye ficheros existentes: exige <see cref="RecoverOptions.ConfirmOverwrite"/>.</summary>
    Overwrite = 5,
}

/// <summary>Respuesta a una pregunta de conflicto.</summary>
public enum RecoverDecision : uint
{
    Rename = 2,
    Skip = 3,
    SkipIfIdentical = 4,
    /// <summary>Sustituir (la respuesta es la confirmación explícita); en directorios, reutilizar.</summary>
    Overwrite = 5,
    /// <summary>Cancela la recuperación.</summary>
    Cancel = 6,
}

public enum RecoverStatus : uint
{
    Unknown = 0,
    Complete = 1,
    Cancelled = 2,
    /// <summary>Se detuvo por un error que impide seguir (destino lleno...).</summary>
    Failed = 3,
}

/// <summary>Opciones de una recuperación (valores por defecto de la adenda de ADR-029).</summary>
public sealed record RecoverOptions
{
    public RecoverConflictPolicy Conflict { get; init; } = RecoverConflictPolicy.Rename;
    /// <summary>Confirmación explícita de <see cref="RecoverConflictPolicy.Overwrite"/>.</summary>
    public bool ConfirmOverwrite { get; init; }
    /// <summary>Estados que se escriben (<c>null</c> = íntegro, íntegro no verificable y parcial).</summary>
    public IReadOnlyCollection<VfsDataState>? WriteStates { get; init; }
    /// <summary>Escribir los bytes del disco en los tramos posiblemente sobrescritos (por defecto, relleno).</summary>
    public bool IncludeReallocatedBytes { get; init; }
    public bool AlternateStreams { get; init; } = true;
    public bool Times { get; init; } = true;
    public bool Attributes { get; init; } = true;
    /// <summary>Fichero <c>.mrbad</c> junto a cada fichero incompleto.</summary>
    public bool MarkIncomplete { get; init; } = true;
    public bool CreateEmptyDirectories { get; init; }
    /// <summary>Filtro de categorías (<c>null</c> = vivo, borrado y huérfano).</summary>
    public IReadOnlyCollection<VfsCategory>? Categories { get; init; }
    /// <summary>Filtro de estados de los datos (<c>null</c> = todos).</summary>
    public IReadOnlyCollection<VfsDataState>? DataStates { get; init; }
    public bool DeletedOnly { get; init; }
    /// <summary>Reglas de nombre <c>regla;regla;!exclusión</c>.</summary>
    public string Masks { get; init; } = "";
    /// <summary>Diario (<c>null</c> = <c>&lt;destino&gt;\MemRescue-registro\recuperacion-*.jsonl</c>).</summary>
    public string? JournalPath { get; init; }
    /// <summary>0 = automática; <see cref="ulong.MaxValue"/> = nunca; otro valor = tamaño de cada parte.</summary>
    public ulong SplitSize { get; init; }
}

/// <summary>Progreso detallado de una recuperación.</summary>
public sealed record RecoverProgress(
    ulong FilesDone, ulong FilesTotal, ulong BytesDone, ulong BytesTotal, ulong UntrustedBytes, ulong Errors,
    bool QuestionPending);

/// <summary>Pregunta de conflicto pendiente.</summary>
public sealed record RecoverQuestion(
    ulong Id, uint Node, bool Directory, bool Deleted, bool ExistingIsDirectory, ulong? ExistingSize,
    ulong NewSize, string Requested, string Existing);

/// <summary>Informe final (el detalle por fichero está en el diario).</summary>
public sealed record RecoverReport(
    RecoverStatus Status, ulong FilesPlanned, ulong DirectoriesPlanned, ulong FilesWritten,
    ulong DirectoriesCreated, ulong DirectoriesMerged, ulong BytesWritten, ulong UntrustedBytes, ulong FilledBytes,
    ulong WrittenIntegro, ulong WrittenIntegroNoVerificable, ulong WrittenParcial, ulong WrittenSobrescrito,
    ulong WrittenOther, ulong SkippedState, ulong SkippedConflict, ulong SkippedIdentical, ulong Unsupported,
    ulong Errors, ulong Renamed, ulong LongPaths, ulong MarkedIncomplete, ulong CancelledFiles,
    bool NamedStreams, bool CaseSensitive, ulong? MaxFileSize, int ExitCode, string JournalPath,
    string Destination, string Reason)
{
    /// <summary>Todo lo seleccionado se escribió y es íntegro.</summary>
    public bool AllIntact => ExitCode == 0;
}

/// <summary>Recuperación en segundo plano. Progreso de la tarea en bytes de los flujos principales.</summary>
public sealed class RecoverTask : EngineTask
{
    internal RecoverTask(ulong handle) : base(handle)
    {
    }

    public RecoverProgress Progress()
    {
        var p = new MrRecoverProgress { StructSize = (uint)Marshal.SizeOf<MrRecoverProgress>() };
        Engine.Check(NativeMethods.mr_recover_progress(Handle, ref p));
        return new RecoverProgress(p.FilesDone, p.FilesTotal, p.BytesDone, p.BytesTotal, p.UntrustedBytes,
            p.Errors, (p.Flags & 1) != 0);
    }

    /// <summary>Ruta relativa en curso.</summary>
    public string Current() =>
        Engine.ReadString((b, c) => (NativeMethods.mr_recover_current(Handle, b, c, out var n), n));

    /// <summary>Pregunta de conflicto pendiente (política <see cref="RecoverConflictPolicy.Ask"/>), o <c>null</c>.</summary>
    public RecoverQuestion? PendingQuestion()
    {
        var q = new MrRecoverQuestion { StructSize = (uint)Marshal.SizeOf<MrRecoverQuestion>() };
        var rc = NativeMethods.mr_recover_question(Handle, ref q);
        if (rc == (int)EngineStatus.NotFound)
        {
            return null;
        }
        Engine.Check(rc);
        try
        {
            var id = q.Id;
            var requested = Engine.ReadString((b, c) =>
                (NativeMethods.mr_recover_question_text(Handle, id, 0, b, c, out var n), n));
            var existing = Engine.ReadString((b, c) =>
                (NativeMethods.mr_recover_question_text(Handle, id, 1, b, c, out var n), n));
            return new RecoverQuestion(q.Id, q.Node, (q.Flags & 1) != 0, (q.Flags & 2) != 0, (q.Flags & 4) != 0,
                q.ExistingSize == ulong.MaxValue ? null : q.ExistingSize, q.NewSize, requested, existing);
        }
        catch (EngineException e) when (e.Status == EngineStatus.NotFound)
        {
            // Respondida entretanto (cancelación).
            return null;
        }
    }

    /// <summary>Responde la pregunta <paramref name="id"/>. <c>false</c> si ya no está pendiente.</summary>
    public bool Answer(ulong id, RecoverDecision decision, bool applyToAll)
    {
        var rc = NativeMethods.mr_recover_answer(Handle, id, (uint)decision, applyToAll ? 1u : 0u);
        if (rc == (int)EngineStatus.NotFound)
        {
            return false;
        }
        Engine.Check(rc);
        return true;
    }

    public RecoverReport GetReport()
    {
        var r = new MrRecoverReport { StructSize = (uint)Marshal.SizeOf<MrRecoverReport>() };
        Engine.Check(NativeMethods.mr_recover_result(Handle, ref r));
        string Text(uint which) =>
            Engine.ReadString((b, c) => (NativeMethods.mr_recover_result_text(Handle, which, b, c, out var n), n));
        return new RecoverReport((RecoverStatus)r.Status, r.FilesPlanned, r.DirsPlanned, r.FilesWritten,
            r.DirsCreated, r.DirsMerged, r.BytesWritten, r.UntrustedBytes, r.FilledBytes, r.WrittenIntegro,
            r.WrittenIntegroNoVerificable, r.WrittenParcial, r.WrittenSobrescrito, r.WrittenOther, r.SkippedState,
            r.SkippedConflict, r.SkippedIdentical, r.Unsupported, r.Errors, r.Renamed, r.LongPaths,
            r.MarkedIncomplete, r.CancelledFiles, (r.Capabilities & 1) != 0, (r.Capabilities & 2) != 0,
            (r.Capabilities & 4) != 0 ? r.MaxFileSize : null, (int)r.ExitCode, Text(0), Text(1), Text(2));
    }
}

public static class RecoverExtensions
{
    private static uint Mask<T>(IReadOnlyCollection<T>? items) where T : Enum =>
        items?.Aggregate(0u, (m, x) => m | (1u << (int)Convert.ToUInt32(x))) ?? 0;

    /// <summary>
    /// Recupera los nodos <paramref name="nodes"/> del árbol (construido desde <paramref name="volume"/>) en
    /// <paramref name="destination"/>. El destino nunca puede ser el origen (ni estar dentro de él) ni un
    /// dispositivo: el motor lo rechaza antes de escribir nada.
    /// </summary>
    public static RecoverTask StartRecover(this VirtualFileSystem vfs, NtfsVolume volume, IReadOnlyList<uint> nodes,
        string destination, RecoverOptions? options = null)
    {
        var o = options ?? new RecoverOptions();
        uint flags = 0;
        if (o.IncludeReallocatedBytes)
        {
            flags |= 1;
        }
        if (!o.AlternateStreams)
        {
            flags |= 2;
        }
        if (!o.Times)
        {
            flags |= 4;
        }
        if (!o.Attributes)
        {
            flags |= 8;
        }
        if (!o.MarkIncomplete)
        {
            flags |= 16;
        }
        if (o.CreateEmptyDirectories)
        {
            flags |= 32;
        }
        if (o.ConfirmOverwrite)
        {
            flags |= 64;
        }
        if (o.DeletedOnly)
        {
            flags |= 128;
        }
        var opts = new MrRecoverOptions
        {
            StructSize = (uint)Marshal.SizeOf<MrRecoverOptions>(),
            Flags = flags,
            Conflict = (uint)o.Conflict,
            WriteStates = Mask(o.WriteStates),
            Categories = Mask(o.Categories),
            DataStates = Mask(o.DataStates),
            SplitSize = o.SplitSize,
        };
        var ids = nodes.ToArray();
        var d = Engine.Utf8(destination);
        var m = Engine.Utf8(o.Masks);
        var j = Engine.Utf8(o.JournalPath ?? "");
        Engine.Check(NativeMethods.mr_recover_start(volume.RawHandle, vfs.RawHandle, ids, (nuint)ids.Length,
            ref opts, d, (nuint)d.Length, m, (nuint)m.Length, j, (nuint)j.Length, out var h));
        return new RecoverTask(h);
    }
}

internal static partial class NativeMethods
{
    [LibraryImport(Dll)]
    internal static partial int mr_recover_start(ulong volume, ulong vfs, uint[] nodes, nuint nodeCount,
        ref MrRecoverOptions options, byte[] dest, nuint destLen, byte[] masks, nuint masksLen, byte[] journal,
        nuint journalLen, out ulong task);

    [LibraryImport(Dll)]
    internal static partial int mr_recover_progress(ulong task, ref MrRecoverProgress progress);

    [LibraryImport(Dll)]
    internal static partial int mr_recover_current(ulong task, byte[]? buf, nuint cap, out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_recover_question(ulong task, ref MrRecoverQuestion question);

    [LibraryImport(Dll)]
    internal static partial int mr_recover_question_text(ulong task, ulong id, uint which, byte[]? buf, nuint cap,
        out nuint outLen);

    [LibraryImport(Dll)]
    internal static partial int mr_recover_answer(ulong task, ulong id, uint decision, uint applyToAll);

    [LibraryImport(Dll)]
    internal static partial int mr_recover_result(ulong task, ref MrRecoverReport report);

    [LibraryImport(Dll)]
    internal static partial int mr_recover_result_text(ulong task, uint which, byte[]? buf, nuint cap,
        out nuint outLen);
}

/// <summary>Espejo de <c>mr_recover_options</c> (48 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrRecoverOptions
{
    public uint StructSize;
    public uint Flags;
    public uint Conflict;
    public uint WriteStates;
    public uint Categories;
    public uint DataStates;
    public ulong SplitSize;
    public uint Fill;
    public uint RenameLimit;
    public ulong Reserved;
}

/// <summary>Espejo de <c>mr_recover_progress</c> (56 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrRecoverProgress
{
    public uint StructSize;
    public uint Flags;
    public ulong FilesDone;
    public ulong FilesTotal;
    public ulong BytesDone;
    public ulong BytesTotal;
    public ulong UntrustedBytes;
    public ulong Errors;
}

/// <summary>Espejo de <c>mr_recover_question</c> (40 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrRecoverQuestion
{
    public uint StructSize;
    public uint Flags;
    public ulong Id;
    public uint Node;
    public uint Reserved;
    public ulong ExistingSize;
    public ulong NewSize;
}

/// <summary>Espejo de <c>mr_recover_report</c> (200 bytes).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct MrRecoverReport
{
    public uint StructSize;
    public uint Status;
    public ulong FilesPlanned;
    public ulong DirsPlanned;
    public ulong FilesWritten;
    public ulong DirsCreated;
    public ulong DirsMerged;
    public ulong BytesWritten;
    public ulong UntrustedBytes;
    public ulong FilledBytes;
    public ulong WrittenIntegro;
    public ulong WrittenIntegroNoVerificable;
    public ulong WrittenParcial;
    public ulong WrittenSobrescrito;
    public ulong WrittenOther;
    public ulong SkippedState;
    public ulong SkippedConflict;
    public ulong SkippedIdentical;
    public ulong Unsupported;
    public ulong Errors;
    public ulong Renamed;
    public ulong LongPaths;
    public ulong MarkedIncomplete;
    public ulong CancelledFiles;
    public uint Capabilities;
    public uint ExitCode;
    public ulong MaxFileSize;
}
