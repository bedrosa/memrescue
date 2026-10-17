using System.Globalization;
using System.Windows;
using MemRescue.Core.Interop;

namespace MemRescue;

/// <summary>Parámetros del modo de autodiagnóstico.</summary>
internal sealed record SelfTestOptions(string Output, string? Image, uint SectorSize, ulong ReadLba, string? Screenshot);

/// <summary>Imagen que se abre al iniciar: <c>--open &lt;ruta&gt; [--sector-size N]</c>.</summary>
internal sealed record OpenOnStartup(string Path, uint SectorSize);

public partial class App : Application
{
    /// <summary>
    /// Autodiagnóstico: <c>MemRescue.exe --selftest &lt;salida.txt&gt; [imagen] [--sector-size N] [--read-lba N] [--screenshot png]</c>.
    /// Abre la ventana real, carga el motor por la DLL y, si se indica una imagen, la abre, muestra y lee la
    /// LBA pedida, calcula hashes y comprueba su legibilidad; escribe el resultado y se cierra.
    /// </summary>
    internal static SelfTestOptions? SelfTest { get; private set; }

    private static SelfTestOptions? ParseSelfTest(IReadOnlyList<string> args)
    {
        var i = args.ToList().IndexOf("--selftest");
        if (i < 0 || i + 1 >= args.Count)
        {
            return null;
        }
        string? image = null, shot = null;
        uint ss = 512;
        ulong lba = 0;
        for (var k = 0; k < args.Count; k++)
        {
            switch (args[k])
            {
                case "--selftest":
                    k++;
                    break;
                case "--sector-size" when k + 1 < args.Count:
                    ss = uint.Parse(args[++k], CultureInfo.InvariantCulture);
                    break;
                case "--read-lba" when k + 1 < args.Count:
                    lba = ulong.Parse(args[++k], CultureInfo.InvariantCulture);
                    break;
                case "--screenshot" when k + 1 < args.Count:
                    shot = args[++k];
                    break;
                default:
                    image ??= args[k];
                    break;
            }
        }
        return new SelfTestOptions(args[i + 1], image, ss, lba, shot);
    }

    /// <summary>
    /// Arranque con imagen: <c>MemRescue.exe --open &lt;ruta&gt; [--sector-size N]</c>. Devuelve <c>null</c> si no se
    /// pidió; lanza <see cref="ArgumentException"/> si los argumentos no son válidos. Se ignora en modo <c>--selftest</c>.
    /// </summary>
    private static OpenOnStartup? ParseOpen(IReadOnlyList<string> args)
    {
        string? path = null;
        uint ss = 0; // automático: nativo del formato (512 en RAW)
        var requested = false;
        for (var k = 0; k < args.Count; k++)
        {
            switch (args[k])
            {
                case "--open":
                    requested = true;
                    if (k + 1 >= args.Count)
                    {
                        throw new ArgumentException("Falta la ruta de la imagen tras --open.");
                    }
                    path = args[++k];
                    break;
                case "--sector-size":
                    if (k + 1 >= args.Count
                        || !uint.TryParse(args[++k], NumberStyles.None, CultureInfo.InvariantCulture, out ss)
                        || ss is not (512 or 1024 or 2048 or 4096))
                    {
                        throw new ArgumentException("--sector-size debe ser 512, 1024, 2048 o 4096.");
                    }
                    break;
            }
        }
        return requested ? new OpenOnStartup(System.IO.Path.GetFullPath(path!), ss) : null;
    }

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        SelfTest = ParseSelfTest(e.Args);
        OpenOnStartup? open = null;
        if (SelfTest is null)
        {
            try
            {
                open = ParseOpen(e.Args);
            }
            catch (ArgumentException ex)
            {
                MessageBox.Show($"Argumentos no válidos: {ex.Message}\n\nUso: MemRescue.exe --open <imagen> [--sector-size N]",
                    "MemRescue", MessageBoxButton.OK, MessageBoxImage.Error);
                Shutdown(2);
                return;
            }
        }

        try
        {
            Engine.EnsureCompatible();
        }
        catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException or EngineException)
        {
            var msg = $"No se pudo cargar el motor de MemRescue (memrescue_core.dll):\n\n{ex.Message}";
            if (SelfTest is not null)
            {
                System.IO.File.WriteAllText(SelfTest.Output, $"ERROR\n{msg}\n");
            }
            else
            {
                MessageBox.Show(msg, "MemRescue", MessageBoxButton.OK, MessageBoxImage.Error);
            }
            Shutdown(1);
            return;
        }

        var window = new MainWindow();
        if (open is not null)
        {
            // Misma lógica que «Abrir imagen…» (interactiva: los errores se muestran al usuario).
            async void OpenWhenRendered(object? sender, EventArgs args)
            {
                window.ContentRendered -= OpenWhenRendered;
                await window.OpenImageAsync(open.Path, open.SectorSize, interactive: true);
            }
            window.ContentRendered += OpenWhenRendered;
        }
        window.Show();
    }
}
