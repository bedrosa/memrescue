using System.Buffers.Binary;
using System.Runtime.InteropServices;
using System.Text;

namespace MemRescue.Core.Interop;

/// <summary>Códigos de estado de la interfaz C (mr_status).</summary>
public enum EngineStatus
{
    Ok = 0,
    InvalidArgument = 1,
    Io = 2,
    PathRejected = 3,
    Store = 4,
    Cancelled = 5,
    NotFinished = 6,
    InvalidHandle = 7,
    BufferTooSmall = 8,
    TaskFailed = 9,
    UnsupportedFormat = 10,
    OutOfRange = 11,
    /// <summary>Estructura del contenedor dañada o incoherente.</summary>
    Corrupt = 12,
    /// <summary>El contenedor necesita otro fichero (padre, extensión) que no se abre.</summary>
    ExternalReference = 13,
    /// <summary>Variante del formato aún no soportada.</summary>
    UnsupportedFeature = 14,
    Panic = 99,
}

/// <summary>Algoritmos de hash (máscara de bits de la ABI).</summary>
[Flags]
public enum HashAlgorithms : uint
{
    None = 0,
    Crc32 = 1,
    Md5 = 2,
    Sha1 = 4,
    Sha256 = 8,
}

public enum EngineTaskState
{
    Running = 1,
    Completed = 2,
    Failed = 3,
    Cancelled = 4,
}

/// <summary>Formato de fichero detectado por sus firmas.</summary>
public enum ImageFormat : uint
{
    Unknown = 0,
    Raw = 1,
    Vhd = 2,
    Vhdx = 3,
    Vmdk = 4,
    VmdkDescriptor = 5,
    Ewf = 6,
    Qcow = 7,
    Vdi = 8,
    Dmg = 9,
}

/// <summary>Estado de validez de un sector entregado por el motor.</summary>
public enum SectorState : byte
{
    /// <summary>Leído al primer intento.</summary>
    Read = 0,
    /// <summary>Leído tras reintentos.</summary>
    Recovered = 1,
    /// <summary>Ilegible: el contenido es un patrón de relleno, NO datos.</summary>
    Unreadable = 2,
    /// <summary>No leído (salto sobre zona dañada): patrón de relleno, NO datos.</summary>
    Skipped = 3,
    /// <summary>No asignado en el contenedor: ceros legítimos.</summary>
    Sparse = 4,
}

public static class SectorStateExtensions
{
    /// <summary><c>true</c> si el contenido representa fielmente el origen.</summary>
    public static bool IsTrustworthy(this SectorState s) => s is not (SectorState.Unreadable or SectorState.Skipped);

    public static string Label(this SectorState s) => s switch
    {
        SectorState.Read => "leído",
        SectorState.Recovered => "recuperado tras reintentos",
        SectorState.Unreadable => "ILEGIBLE: bytes de relleno, no datos",
        SectorState.Skipped => "SALTADO (no leído): bytes de relleno, no datos",
        SectorState.Sparse => "disperso (ceros del formato)",
        _ => "desconocido",
    };
}

/// <summary>Error devuelto por el motor, con su código y el mensaje de mr_last_error.</summary>
public sealed class EngineException(EngineStatus status, string message) : Exception(message)
{
    public EngineStatus Status { get; } = status;
}

public readonly record struct ImageInfo(
    string Path, ulong SizeBytes, uint SectorSize, ulong SectorCount, ulong TrailingBytes,
    ImageFormat Format = ImageFormat.Raw);

public readonly record struct TaskProgress(ulong Done, ulong Total)
{
    public double Fraction => Total == 0 ? 0 : (double)Done / Total;
}

/// <summary>Tramo no fiable (ilegible o saltado) del mapa de validez.</summary>
public readonly record struct UntrustedRange(ulong Lba, ulong Count, SectorState State);

public sealed record VerifyReport(
    ulong SectorsRequested, ulong SectorsRead, ulong SectorsRecovered, ulong SectorsUnreadable,
    ulong SectorsSkipped, ulong SectorsSparse, ulong RetryReads,
    IReadOnlyList<UntrustedRange> UntrustedRanges, bool RangesTruncated)
{
    /// <summary>Sectores cuyo contenido representa fielmente el origen.</summary>
    public ulong SectorsTrustworthy => SectorsRead + SectorsRecovered + SectorsSparse;

    /// <summary><c>true</c> si ningún sector es ilegible ni se saltó.</summary>
    public bool AllTrustworthy => SectorsUnreadable == 0 && SectorsSkipped == 0;

    /// <summary>Sectores ilegibles cuyo error se omitió por la política (sin agotar reintentos).</summary>
    public ulong OmittedErrors { get; init; }

    /// <summary>Códigos de error del sistema de los sectores ilegibles, en orden creciente.</summary>
    public IReadOnlyList<ErrorCodeCount> ErrorCodes { get; init; } = [];
}

/// <summary>Código de error del sistema y número de sectores ilegibles cuyo último intento falló con él.</summary>
public readonly record struct ErrorCodeCount(int Code, ulong Sectors);

/// <summary>Sectores leídos con el estado de cada uno.</summary>
public sealed record MappedSectors(byte[] Data, SectorState[] States)
{
    public bool AllTrustworthy => States.All(s => s.IsTrustworthy());
}

/// <summary>Punto de entrada seguro al motor de MemRescue.</summary>
public static class Engine
{
    /// <summary>Tamaños de sector lógico ofrecidos en la interfaz.</summary>
    public static IReadOnlyList<uint> CommonSectorSizes { get; } = [512, 1024, 2048, 4096];

    /// <summary>Versión de la ABI que expone la DLL cargada.</summary>
    public static uint AbiVersion => NativeMethods.mr_abi_version();

    public static uint ExpectedAbiVersion => NativeMethods.ExpectedAbiVersion;

    /// <summary>Lanza si la DLL no es compatible con este ensamblado.</summary>
    public static void EnsureCompatible()
    {
        var abi = AbiVersion;
        if (abi != NativeMethods.ExpectedAbiVersion)
        {
            throw new EngineException(EngineStatus.InvalidArgument,
                $"memrescue_core.dll expone la ABI {abi}; se esperaba la {NativeMethods.ExpectedAbiVersion}.");
        }
    }

    public static string EngineVersion => ReadString((b, c) => (NativeMethods.mr_engine_version(b, c, out var n), n));

    /// <summary>Detecta el formato de un fichero (RAW o contenedor) sin abrirlo como origen.</summary>
    public static ImageFormat Probe(string path)
    {
        var p = Utf8(path);
        Check(NativeMethods.mr_image_probe(p, (nuint)p.Length, out var f));
        return Enum.IsDefined(typeof(ImageFormat), f) ? (ImageFormat)f : ImageFormat.Unknown;
    }

    public static ImageInfo GetImageInfo(string path, uint sectorSize = 0)
    {
        var p = Utf8(path);
        Check(NativeMethods.mr_image_info_get(p, (nuint)p.Length, sectorSize, out var i));
        return ToInfo(path, i);
    }

    /// <summary>Abre una imagen (RAW, VHD, VHDX o VMDK monolithicSparse) en solo lectura.</summary>
    /// <param name="path">Fichero de imagen; las rutas de dispositivo se rechazan.</param>
    /// <param name="sectorSize">Tamaño de sector lógico (potencia de 2, 512–65536); 0 = nativo del formato.</param>
    /// <param name="forceRaw">Interpretar como RAW aunque parezca un contenedor (VHD, VMDK…).</param>
    /// <param name="split">Abrir la imagen dividida que empieza en <paramref name="path"/> (x.001).</param>
    /// <param name="tolerant">
    /// Modo tolerante para VHD, VHDX y VMDK con tablas de asignación parcialmente dañadas (solo por
    /// petición explícita del usuario): los bloques con entradas inválidas o ambiguas se descartan y
    /// se leen como ilegibles (patrón de relleno, nunca datos); <see cref="Source.Description"/> lo
    /// indica con el número de bloques descartados. Sin él, esas imágenes fallan con
    /// <see cref="EngineStatus.Corrupt"/>.
    /// </param>
    public static Source OpenSource(string path, uint sectorSize = 0, bool forceRaw = false, bool split = false,
        bool tolerant = false)
    {
        var p = Utf8(path);
        var flags = (forceRaw ? NativeMethods.OpenForceRaw : 0) | (split ? NativeMethods.OpenSplit : 0)
            | (tolerant ? NativeMethods.OpenTolerant : 0);
        Check(NativeMethods.mr_source_open(p, (nuint)p.Length, sectorSize, flags, out var h));
        return new Source(h, path);
    }

    /// <summary>Lista los segmentos de una imagen dividida (x.001, x.002…) sin abrirlos.</summary>
    public static IReadOnlyList<string> DiscoverSplit(string firstSegment)
    {
        var p = Utf8(firstSegment);
        var text = ReadString((b, c) => (NativeMethods.mr_split_discover(p, (nuint)p.Length, b, c, out var n, out _), n));
        return text.Split('\n', StringSplitOptions.RemoveEmptyEntries);
    }

    internal static ImageInfo ToInfo(string path, MrImageInfo i) =>
        new(path, i.SizeBytes, i.SectorSize, i.SectorCount, i.TrailingBytes,
            Enum.IsDefined(typeof(ImageFormat), i.Format) ? (ImageFormat)i.Format : ImageFormat.Unknown);

    /// <summary>Inicia el cálculo de hashes de una imagen en segundo plano (solo lectura).</summary>
    /// <param name="path">Fichero de imagen; las rutas de dispositivo se rechazan.</param>
    /// <param name="algorithms">Uno o varios algoritmos.</param>
    /// <param name="sectorSize">Tamaño de sector lógico (potencia de 2, 512–65536).</param>
    /// <param name="storePath">Base de datos de trabajo SQLite opcional donde registrar el trabajo.</param>
    public static HashTask StartHash(string path, HashAlgorithms algorithms, uint sectorSize = 0, string? storePath = null)
    {
        var p = Utf8(path);
        var s = storePath is null ? null : Utf8(storePath);
        Check(NativeMethods.mr_hash_start(p, (nuint)p.Length, sectorSize, (uint)algorithms, s, (nuint)(s?.Length ?? 0), out var h));
        return new HashTask(h, algorithms);
    }

    internal static byte[] Utf8(string s) => Encoding.UTF8.GetBytes(s);

    /// <summary>Convierte un código de error en excepción, leyendo el mensaje en el mismo hilo.</summary>
    internal static void Check(int rc)
    {
        if (rc == (int)EngineStatus.Ok)
        {
            return;
        }
        throw new EngineException((EngineStatus)rc, LastError());
    }

    internal static string LastError()
    {
        try
        {
            return ReadString((b, c) => (NativeMethods.mr_last_error(b, c, out var n), n), check: false);
        }
        catch (Exception e)
        {
            return $"(no se pudo leer el error del motor: {e.Message})";
        }
    }

    /// <summary>Lee una cadena de salida con el protocolo (buf, cap, out_len) de la ABI.</summary>
    internal static string ReadString(Func<byte[], nuint, (int rc, nuint len)> call, bool check = true)
    {
        var buf = new byte[256];
        for (var attempt = 0; attempt < 2; attempt++)
        {
            var (rc, len) = call(buf, (nuint)buf.Length);
            if (rc == (int)EngineStatus.Ok)
            {
                return Encoding.UTF8.GetString(buf, 0, checked((int)len));
            }
            if (rc == (int)EngineStatus.BufferTooSmall)
            {
                buf = new byte[checked((int)len + 1)];
                continue;
            }
            if (check)
            {
                Check(rc);
            }
            return string.Empty;
        }
        throw new EngineException(EngineStatus.BufferTooSmall, "La longitud de la cadena del motor cambió entre llamadas.");
    }
}

/// <summary>
/// Handle del motor con liberación garantizada: <see cref="IDisposable"/> y, como red de seguridad,
/// finalizador (si el código olvida liberarlo, el GC lo hace y el registro del motor no crece).
/// </summary>
public abstract class EngineHandle : IDisposable
{
    private ulong _handle;

    private protected EngineHandle(ulong handle) => _handle = handle;

    ~EngineHandle() => Release(Interlocked.Exchange(ref _handle, 0));

    private protected ulong Handle =>
        Volatile.Read(ref _handle) is var h && h != 0 ? h : throw new ObjectDisposedException(GetType().Name);

    public bool IsDisposed => Volatile.Read(ref _handle) == 0;

    private protected abstract void Release(ulong handle);

    public void Dispose()
    {
        Release(Interlocked.Exchange(ref _handle, 0));
        GC.SuppressFinalize(this);
    }
}

/// <summary>Origen abierto en el motor (imagen, contenedor, imagen dividida o rango; solo lectura).</summary>
public sealed class Source : EngineHandle
{
    internal Source(ulong handle, string path, ulong? rangeStart = null) : base(handle)
    {
        Path = path;
        RangeStart = rangeStart;
        Engine.Check(NativeMethods.mr_source_info(Handle, out var i));
        Info = Engine.ToInfo(path, i);
        Description = Engine.ReadString((b, c) => (NativeMethods.mr_source_describe(Handle, b, c, out var n), n));
    }

    public string Path { get; }

    public ImageInfo Info { get; }

    /// <summary>Descripción legible (variante del contenedor, segmentos, rango…).</summary>
    public string Description { get; }

    /// <summary>Si es un rango de otro origen, su LBA inicial en el padre.</summary>
    public ulong? RangeStart { get; }

    /// <summary>Abre <paramref name="count"/> sectores desde <paramref name="startLba"/> como un origen independiente.</summary>
    public Source OpenRange(ulong startLba, ulong count)
    {
        Engine.Check(NativeMethods.mr_source_open_range(Handle, startLba, count, out var h));
        return new Source(h, Path, startLba);
    }

    /// <summary>
    /// Lee sectores tolerando errores, con el estado de cada uno. Los ilegibles llegan con un patrón
    /// de relleno que NUNCA debe presentarse como datos: consulte <see cref="MappedSectors.States"/>.
    /// </summary>
    /// <param name="lba">Primer sector.</param>
    /// <param name="count">Nº de sectores (al menos 1).</param>
    /// <param name="retries">Reintentos por sector ilegible (0–100).</param>
    /// <param name="badFill">Relleno de los ilegibles; <c>null</c> = <see cref="FillPattern.DefaultBad"/>.</param>
    /// <param name="skipFill">Relleno de los saltados; <c>null</c> = <see cref="FillPattern.DefaultSkip"/>.</param>
    /// <param name="jumpSectors">Tras un ilegible, sectores que se marcan saltados sin leerlos (0 = sin salto).</param>
    /// <param name="readBack">Tras un salto, leer hacia atrás hasta el siguiente error.</param>
    /// <remarks>
    /// Los patrones deben cumplir <see cref="FillPattern.Validate"/>; si no, el motor responde con
    /// <see cref="EngineStatus.InvalidArgument"/>. Sin patrones ni salto se usa
    /// <c>mr_source_read_mapped</c>; en otro caso, <c>mr_source_read_mapped_ex</c>.
    /// </remarks>
    public MappedSectors ReadSectorsMapped(ulong lba, uint count, uint retries = 1, FillPattern? badFill = null,
        FillPattern? skipFill = null, ulong jumpSectors = 0, bool readBack = false)
    {
        if (count == 0)
        {
            throw new ArgumentOutOfRangeException(nameof(count));
        }
        var len = checked((int)(count * Info.SectorSize));
        var buf = new byte[len];
        var states = new byte[count];
        nuint n;
        if (badFill is null && skipFill is null && jumpSectors == 0 && !readBack)
        {
            Engine.Check(NativeMethods.mr_source_read_mapped(Handle, lba, count, retries, buf, (nuint)buf.Length, states, (nuint)states.Length, out n));
        }
        else
        {
            var o = new MrReadOptions
            {
                StructSize = (uint)Marshal.SizeOf<MrReadOptions>(),
                Flags = readBack ? NativeMethods.ReadReadBack : 0,
                JumpSectors = jumpSectors,
                Retries = retries,
                BadFill = BinaryPrimitives.ReadUInt32LittleEndian((badFill ?? FillPattern.DefaultBad).ToArray()),
                SkipFill = BinaryPrimitives.ReadUInt32LittleEndian((skipFill ?? FillPattern.DefaultSkip).ToArray()),
            };
            Engine.Check(NativeMethods.mr_source_read_mapped_ex(Handle, lba, count, in o, buf, (nuint)buf.Length, states, (nuint)states.Length, out n));
        }
        if ((int)n != len)
        {
            throw new EngineException(EngineStatus.Io, $"Lectura incompleta: {n} de {len} bytes.");
        }
        return new MappedSectors(buf, states.Select(s => (SectorState)s).ToArray());
    }

    /// <summary>Lee <paramref name="count"/> sectores completos desde <paramref name="lba"/>.</summary>
    public byte[] ReadSectors(ulong lba, uint count)
    {
        if (count == 0)
        {
            throw new ArgumentOutOfRangeException(nameof(count));
        }
        var len = checked((int)(count * Info.SectorSize));
        var buf = new byte[len];
        Engine.Check(NativeMethods.mr_source_read(Handle, lba, count, buf, (nuint)buf.Length, out var n));
        if ((int)n != len)
        {
            throw new EngineException(EngineStatus.Io, $"Lectura incompleta: {n} de {len} bytes.");
        }
        return buf;
    }

    /// <summary>
    /// Lee la tabla de particiones del origen (MBR/EBR; MBR protector o híbrido de GPT). La lectura es
    /// tolerante: un sector ilegible aparece como anomalía, nunca se interpreta su relleno.
    /// </summary>
    public PartitionTable ReadPartitions()
    {
        // Si la lectura de las listas falla, el finalizador de la tabla libera el handle.
        Engine.Check(NativeMethods.mr_source_partitions(Handle, out var h));
        return new PartitionTable(h, this);
    }

    /// <summary>
    /// Lanza la búsqueda exprés de particiones perdidas (solo a petición). Lee la tabla del origen y
    /// sondea un número acotado de posiciones candidatas; nunca recorre el disco entero.
    /// </summary>
    public PartitionSearchTask StartPartitionSearch(PartitionSearchOptions? options = null)
    {
        var o = options ?? new PartitionSearchOptions();
        var native = new MrPartitionSearchOptions
        {
            StructSize = (uint)Marshal.SizeOf<MrPartitionSearchOptions>(),
            Families = (uint)o.Families,
            MaxReads = o.MaxReads,
            MaxBytes = o.MaxBytes,
            StartGridBytes = o.StartGridBytes,
            EndGridBytes = o.EndGridBytes,
            MaxVolumes = o.MaxVolumes,
            ProbeWindow = o.ProbeWindow,
            Cylinders = o.Cylinders,
            ProbesPerGap = o.ProbesPerGap,
        };
        Engine.Check(NativeMethods.mr_source_partition_search_start(Handle, in native, out var h));
        return new PartitionSearchTask(h, this);
    }

    public HashTask StartHash(HashAlgorithms algorithms, string? storePath = null)
    {
        var s = storePath is null ? null : Engine.Utf8(storePath);
        Engine.Check(NativeMethods.mr_source_hash_start(Handle, (uint)algorithms, s, (nuint)(s?.Length ?? 0), out var h));
        return new HashTask(h, algorithms);
    }

    /// <summary>Lee todos los sectores (o un rango) tolerando errores e informa del estado de cada uno.</summary>
    /// <param name="startLba">Primer sector.</param>
    /// <param name="count">Nº de sectores; 0 = hasta el final.</param>
    /// <param name="retries">Reintentos por sector ilegible (0–100).</param>
    /// <param name="jumpSectors">Tras un sector ilegible, saltar estos sectores sin leerlos (0 = no saltar).</param>
    /// <param name="readBack">Tras un salto, leer hacia atrás hasta el siguiente error.</param>
    /// <param name="skipErrorCodes">
    /// Códigos de error del sistema (p. ej. 23, 1117) que no se reintentan: el sector queda ilegible
    /// al primer fallo con uno de ellos. Máximo 256; deben ser positivos.
    /// </param>
    /// <param name="ignoreAllErrors">Modo «ignorar todo»: ningún error se reintenta.</param>
    public VerifyTask StartVerify(ulong startLba = 0, ulong count = 0, uint retries = 1, ulong jumpSectors = 0, bool readBack = false,
        IReadOnlyCollection<int>? skipErrorCodes = null, bool ignoreAllErrors = false)
    {
        var flags = readBack ? NativeMethods.VerifyReadBack : 0;
        if ((skipErrorCodes is null || skipErrorCodes.Count == 0) && !ignoreAllErrors)
        {
            Engine.Check(NativeMethods.mr_source_verify_start(Handle, startLba, count, retries, jumpSectors, flags, out var h0));
            return new VerifyTask(h0);
        }
        var codes = skipErrorCodes?.ToArray() ?? [];
        var pin = GCHandle.Alloc(codes, GCHandleType.Pinned);
        try
        {
            var o = new MrVerifyOptions
            {
                StructSize = (uint)Marshal.SizeOf<MrVerifyOptions>(),
                Flags = flags | (ignoreAllErrors ? NativeMethods.VerifyIgnoreAllErrors : 0),
                StartLba = startLba,
                Count = count,
                JumpSectors = jumpSectors,
                Retries = retries,
                SkipCodeCount = (uint)codes.Length,
                SkipCodes = codes.Length == 0 ? 0 : pin.AddrOfPinnedObject(),
            };
            Engine.Check(NativeMethods.mr_source_verify_start_ex(Handle, in o, out var h));
            return new VerifyTask(h);
        }
        finally
        {
            pin.Free();
        }
    }

    private protected override void Release(ulong handle)
    {
        if (handle != 0)
        {
            NativeMethods.mr_source_close(handle);
        }
    }
}

/// <summary>Tarea del motor (sondeo de estado y cancelación cooperativa).</summary>
public abstract class EngineTask : EngineHandle
{
    private protected EngineTask(ulong handle) : base(handle)
    {
    }

    public (EngineTaskState State, TaskProgress Progress) Status()
    {
        Engine.Check(NativeMethods.mr_task_status_get(Handle, out var s));
        return ((EngineTaskState)s.State, new TaskProgress(s.Done, s.Total));
    }

    public void Cancel() => Engine.Check(NativeMethods.mr_task_cancel(Handle));

    /// <summary>
    /// Espera a que termine consultando el estado periódicamente. Si se cancela el token, pide la
    /// cancelación al motor y sigue esperando hasta que el motor confirme el final.
    /// </summary>
    public async Task<EngineTaskState> WaitAsync(IProgress<TaskProgress>? progress = null,
        CancellationToken cancellationToken = default, TimeSpan? pollInterval = null)
    {
        var delay = pollInterval ?? TimeSpan.FromMilliseconds(100);
        var cancelRequested = false;
        while (true)
        {
            var (state, p) = Status();
            progress?.Report(p);
            if (state != EngineTaskState.Running)
            {
                return state;
            }
            if (cancellationToken.IsCancellationRequested && !cancelRequested)
            {
                Cancel();
                cancelRequested = true;
            }
            await Task.Delay(delay, CancellationToken.None).ConfigureAwait(false);
        }
    }

    private protected override void Release(ulong handle)
    {
        if (handle != 0)
        {
            NativeMethods.mr_task_free(handle);
        }
    }
}

/// <summary>Tarea de hash. El progreso se expresa en bytes.</summary>
public sealed class HashTask : EngineTask
{
    internal HashTask(ulong handle, HashAlgorithms algorithms) : base(handle) => Algorithms = algorithms;

    public HashAlgorithms Algorithms { get; }

    /// <summary>Resumen hexadecimal de un algoritmo (la tarea debe haber terminado).</summary>
    public string GetResult(HashAlgorithms algorithm) =>
        Engine.ReadString((b, c) => (NativeMethods.mr_hash_result(Handle, (uint)algorithm, b, c, out var n), n));
}

/// <summary>Tarea de verificación de legibilidad. El progreso se expresa en sectores.</summary>
public sealed class VerifyTask : EngineTask
{
    internal VerifyTask(ulong handle) : base(handle)
    {
    }

    public VerifyReport GetReport()
    {
        Engine.Check(NativeMethods.mr_verify_result(Handle, out var r));
        var ranges = new List<UntrustedRange>(checked((int)Math.Min(r.UntrustedRangeCount, 100_000)));
        for (ulong i = 0; i < r.UntrustedRangeCount; i++)
        {
            Engine.Check(NativeMethods.mr_verify_untrusted_range(Handle, i, out var lba, out var count, out var state));
            ranges.Add(new UntrustedRange(lba, count, (SectorState)state));
        }
        Engine.Check(NativeMethods.mr_verify_error_summary(Handle, out var omitted, out var codeCount));
        var codes = new List<ErrorCodeCount>((int)Math.Min(codeCount, 1024));
        for (uint i = 0; i < codeCount; i++)
        {
            Engine.Check(NativeMethods.mr_verify_error_code(Handle, i, out var code, out var sectors));
            codes.Add(new ErrorCodeCount(code, sectors));
        }
        return new VerifyReport(r.SectorsRequested, r.SectorsRead, r.SectorsRecovered, r.SectorsUnreadable,
            r.SectorsSkipped, r.SectorsSparse, r.RetryReads, ranges, r.RangesTruncated != 0)
        {
            OmittedErrors = omitted,
            ErrorCodes = codes,
        };
    }
}
