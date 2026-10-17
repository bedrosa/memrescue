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

/// <summary>Error devuelto por el motor, con su código y el mensaje de mr_last_error.</summary>
public sealed class EngineException(EngineStatus status, string message) : Exception(message)
{
    public EngineStatus Status { get; } = status;
}

public readonly record struct ImageInfo(string Path, ulong SizeBytes, uint SectorSize, ulong SectorCount, ulong TrailingBytes);

public readonly record struct TaskProgress(ulong Done, ulong Total)
{
    public double Fraction => Total == 0 ? 0 : (double)Done / Total;
}

public readonly record struct BadRange(ulong Lba, ulong Count);

public sealed record VerifyReport(
    ulong SectorsRequested, ulong SectorsOk, ulong SectorsBad, ulong RetryReads,
    IReadOnlyList<BadRange> BadRanges, bool BadRangesTruncated);

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

    public static ImageInfo GetImageInfo(string path, uint sectorSize = 512)
    {
        var p = Utf8(path);
        Check(NativeMethods.mr_image_info_get(p, (nuint)p.Length, sectorSize, out var i));
        return new ImageInfo(path, i.SizeBytes, i.SectorSize, i.SectorCount, i.TrailingBytes);
    }

    /// <summary>Abre una imagen RAW en solo lectura.</summary>
    /// <param name="path">Fichero de imagen; las rutas de dispositivo se rechazan.</param>
    /// <param name="sectorSize">Tamaño de sector lógico (potencia de 2, 512–65536).</param>
    /// <param name="forceRaw">Interpretar como RAW aunque parezca un contenedor (VHD, VMDK…).</param>
    public static Source OpenSource(string path, uint sectorSize = 512, bool forceRaw = false)
    {
        var p = Utf8(path);
        Check(NativeMethods.mr_source_open(p, (nuint)p.Length, sectorSize, forceRaw ? 1u : 0u, out var h));
        return new Source(h, path);
    }

    /// <summary>Inicia el cálculo de hashes de una imagen en segundo plano (solo lectura).</summary>
    /// <param name="path">Fichero de imagen; las rutas de dispositivo se rechazan.</param>
    /// <param name="algorithms">Uno o varios algoritmos.</param>
    /// <param name="sectorSize">Tamaño de sector lógico (potencia de 2, 512–65536).</param>
    /// <param name="storePath">Base de datos de trabajo SQLite opcional donde registrar el trabajo.</param>
    public static HashTask StartHash(string path, HashAlgorithms algorithms, uint sectorSize = 512, string? storePath = null)
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

/// <summary>Origen abierto en el motor (imagen RAW, solo lectura).</summary>
public sealed class Source : EngineHandle
{
    internal Source(ulong handle, string path) : base(handle)
    {
        Path = path;
        Engine.Check(NativeMethods.mr_source_info(Handle, out var i));
        Info = new ImageInfo(path, i.SizeBytes, i.SectorSize, i.SectorCount, i.TrailingBytes);
    }

    public string Path { get; }

    public ImageInfo Info { get; }

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

    public HashTask StartHash(HashAlgorithms algorithms, string? storePath = null)
    {
        var s = storePath is null ? null : Engine.Utf8(storePath);
        Engine.Check(NativeMethods.mr_source_hash_start(Handle, (uint)algorithms, s, (nuint)(s?.Length ?? 0), out var h));
        return new HashTask(h, algorithms);
    }

    /// <summary>Lee todos los sectores (o un rango) tolerando errores e informa de los ilegibles.</summary>
    /// <param name="startLba">Primer sector.</param>
    /// <param name="count">Nº de sectores; 0 = hasta el final.</param>
    /// <param name="retries">Reintentos por sector ilegible (0–100).</param>
    public VerifyTask StartVerify(ulong startLba = 0, ulong count = 0, uint retries = 1)
    {
        Engine.Check(NativeMethods.mr_source_verify_start(Handle, startLba, count, retries, out var h));
        return new VerifyTask(h);
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
        var ranges = new List<BadRange>(checked((int)Math.Min(r.BadRangeCount, 100_000)));
        for (ulong i = 0; i < r.BadRangeCount; i++)
        {
            Engine.Check(NativeMethods.mr_verify_bad_range(Handle, i, out var lba, out var count));
            ranges.Add(new BadRange(lba, count));
        }
        return new VerifyReport(r.SectorsRequested, r.SectorsOk, r.SectorsBad, r.RetryReads, ranges, r.BadRangesTruncated != 0);
    }
}
