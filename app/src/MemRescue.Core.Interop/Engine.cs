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

/// <summary>Punto de entrada seguro al motor de MemRescue.</summary>
public static class Engine
{
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

    public static ImageInfo GetImageInfo(string path, uint sectorSize = 512)
    {
        var p = Utf8(path);
        Check(NativeMethods.mr_image_info_get(p, (nuint)p.Length, sectorSize, out var i));
        return new ImageInfo(path, i.SizeBytes, i.SectorSize, i.SectorCount, i.TrailingBytes);
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
/// Tarea de hash del motor. Propiedad: el handle pertenece a esta instancia y se libera con Dispose
/// (si la tarea sigue en marcha, el motor la cancela).
/// </summary>
public sealed class HashTask : IDisposable
{
    private ulong _handle;

    internal HashTask(ulong handle, HashAlgorithms algorithms)
    {
        _handle = handle;
        Algorithms = algorithms;
    }

    public HashAlgorithms Algorithms { get; }

    private ulong Handle => _handle != 0 ? _handle : throw new ObjectDisposedException(nameof(HashTask));

    public (EngineTaskState State, TaskProgress Progress) Status()
    {
        Engine.Check(NativeMethods.mr_task_status_get(Handle, out var s));
        return ((EngineTaskState)s.State, new TaskProgress(s.Done, s.Total));
    }

    public void Cancel() => Engine.Check(NativeMethods.mr_task_cancel(Handle));

    /// <summary>Resumen hexadecimal de un algoritmo (la tarea debe haber terminado).</summary>
    public string GetResult(HashAlgorithms algorithm) =>
        Engine.ReadString((b, c) => (NativeMethods.mr_hash_result(Handle, (uint)algorithm, b, c, out var n), n));

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

    public void Dispose()
    {
        var h = Interlocked.Exchange(ref _handle, 0);
        if (h != 0)
        {
            NativeMethods.mr_task_free(h);
        }
    }
}
