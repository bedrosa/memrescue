using System.Windows;
using MemRescue.Core.Interop;

namespace MemRescue;

public partial class App : Application
{
    /// <summary>
    /// Modo de autodiagnóstico: <c>MemRescue.exe --selftest &lt;salida.txt&gt; [imagen]</c>.
    /// Abre la ventana real, consulta el motor a través de la DLL, opcionalmente calcula el SHA-256
    /// de una imagen, escribe el resultado y se cierra. Lo usa la verificación automática de M0.
    /// </summary>
    internal static string? SelfTestOutput { get; private set; }

    internal static string? SelfTestImage { get; private set; }

    /// <summary>Con <c>--screenshot &lt;png&gt;</c> el autodiagnóstico guarda una captura de la ventana.</summary>
    internal static string? ScreenshotPath { get; private set; }

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        var args = e.Args.ToList();
        var shot = args.IndexOf("--screenshot");
        if (shot >= 0 && shot + 1 < args.Count)
        {
            ScreenshotPath = args[shot + 1];
            args.RemoveRange(shot, 2);
        }
        if (args.Count >= 2 && args[0] == "--selftest")
        {
            SelfTestOutput = args[1];
            SelfTestImage = args.Count >= 3 ? args[2] : null;
        }

        try
        {
            Engine.EnsureCompatible();
        }
        catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException or EngineException)
        {
            var msg = $"No se pudo cargar el motor de MemRescue (memrescue_core.dll):\n\n{ex.Message}";
            if (SelfTestOutput is not null)
            {
                System.IO.File.WriteAllText(SelfTestOutput, $"ERROR\n{msg}\n");
            }
            else
            {
                MessageBox.Show(msg, "MemRescue", MessageBoxButton.OK, MessageBoxImage.Error);
            }
            Shutdown(1);
            return;
        }

        new MainWindow().Show();
    }
}
