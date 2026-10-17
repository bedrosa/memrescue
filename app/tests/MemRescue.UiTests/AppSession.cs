using System.Diagnostics;
using System.Globalization;
using System.Text.RegularExpressions;
using System.Windows.Automation;

namespace MemRescue.UiTests;

/// <summary>
/// Una instancia real de <c>MemRescue.exe</c> lanzada con <c>--open</c> y manejada por UI Automation desde
/// fuera del proceso. <see cref="Dispose"/> cierra la ventana y, si no responde, mata el proceso: no quedan
/// procesos huérfanos aunque la prueba falle.
/// </summary>
internal sealed partial class AppSession : IDisposable
{
    public static readonly TimeSpan DefaultTimeout = TimeSpan.FromSeconds(20);

    /// <summary>
    /// Límite para el arranque: el primer lanzamiento de un ejecutable recién compilado puede tardar bastante
    /// más (análisis antivirus, JIT en frío).
    /// </summary>
    public static readonly TimeSpan StartupTimeout = TimeSpan.FromSeconds(90);

    private readonly Process _process;

    public AutomationElement Window { get; }

    private AppSession(Process process, AutomationElement window)
    {
        _process = process;
        Window = window;
    }

    /// <summary>Lanza MemRescue con la imagen indicada y espera a que el visor muestre la LBA 0.</summary>
    public static AppSession Launch(string image, uint? sectorSize = null)
    {
        var psi = new ProcessStartInfo(RepoPaths.MemRescueExe) { UseShellExecute = false };
        psi.ArgumentList.Add("--open");
        psi.ArgumentList.Add(image);
        if (sectorSize is { } ss)
        {
            psi.ArgumentList.Add("--sector-size");
            psi.ArgumentList.Add(ss.ToString(CultureInfo.InvariantCulture));
        }
        var process = Process.Start(psi) ?? throw new InvalidOperationException("No se pudo lanzar MemRescue.exe.");
        try
        {
            var cond = new AndCondition(
                new PropertyCondition(AutomationElement.ProcessIdProperty, process.Id),
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Window));
            var window = WaitFor(() =>
            {
                if (process.HasExited)
                {
                    throw new InvalidOperationException($"MemRescue.exe terminó antes de mostrar la ventana (código {process.ExitCode}).");
                }
                var w = AutomationElement.RootElement.FindFirst(TreeScope.Children, cond);
                return w is not null && w.Current.Name == "MemRescue" ? w : null;
            }, "la ventana «MemRescue»", StartupTimeout);
            var session = new AppSession(process, window);
            // La imagen se abre al terminar de renderizarse la ventana: se espera a que aparezca el visor.
            session.WaitUntil(() => session.HexText.Contains("── LBA 0 ·", StringComparison.Ordinal), "el visor con la LBA 0 de la imagen", StartupTimeout);
            return session;
        }
        catch
        {
            KillQuietly(process);
            process.Dispose();
            throw;
        }
    }

    // ---------- Esperas con sondeo ----------

    /// <summary>Sondea <paramref name="probe"/> hasta que devuelva un valor no nulo o venza el tiempo límite.</summary>
    public static T WaitFor<T>(Func<T?> probe, string what, TimeSpan? timeout = null)
        where T : class
    {
        var limit = timeout ?? DefaultTimeout;
        var sw = Stopwatch.StartNew();
        Exception? last = null;
        while (true)
        {
            try
            {
                if (probe() is { } r)
                {
                    return r;
                }
            }
            catch (ElementNotAvailableException ex)
            {
                last = ex; // el árbol cambia mientras se recorre; se reintenta
            }
            if (sw.Elapsed > limit)
            {
                throw new TimeoutException($"Tiempo agotado ({limit.TotalSeconds:0} s) esperando {what}.", last);
            }
            Thread.Sleep(50);
        }
    }

    public void WaitUntil(Func<bool> condition, string what, TimeSpan? timeout = null) =>
        WaitFor(() => condition() ? this : null, what, timeout);

    // ---------- Acceso a controles ----------

    public AutomationElement ById(string automationId) =>
        WaitFor(() => Window.FindFirst(TreeScope.Descendants,
            new PropertyCondition(AutomationElement.AutomationIdProperty, automationId)), $"el control «{automationId}»");

    /// <summary>Texto completo del visor hexadecimal (<c>HexView</c>).</summary>
    public string HexText => ((ValuePattern)ById("HexView").GetCurrentPattern(ValuePattern.Pattern)).Current.Value;

    public string SectorInfo => ById("SectorInfoText").Current.Name;

    public string Status => ById("StatusText").Current.Name;

    public string Position => ById("PositionText").Current.Name;

    /// <summary>LBA mostrada en <c>SectorInfoText</c> («LBA n de m …»), independiente del separador de miles.</summary>
    public ulong? ShownLba
    {
        get
        {
            var m = ShownLbaRegex().Match(SectorInfo);
            return m.Success ? ulong.Parse(DigitsOnly(m.Groups[1].Value), CultureInfo.InvariantCulture) : null;
        }
    }

    private static string DigitsOnly(string s) => new([.. s.Where(char.IsAsciiDigit)]);

    [GeneratedRegex(@"^LBA (\d[\d\W]*?) de ")]
    private static partial Regex ShownLbaRegex();

    public void Invoke(string automationId) =>
        ((InvokePattern)ById(automationId).GetCurrentPattern(InvokePattern.Pattern)).Invoke();

    public void SetText(string automationId, string text) =>
        ((ValuePattern)ById(automationId).GetCurrentPattern(ValuePattern.Pattern)).SetValue(text);

    /// <summary>Navega y espera a que el visor muestre la LBA <paramref name="lba"/>.</summary>
    public void WaitForLba(ulong lba)
    {
        try
        {
            WaitUntil(() => ShownLba == lba && HexLbaMatches(lba), $"el visor en la LBA {lba}");
        }
        catch (TimeoutException ex)
        {
            throw new TimeoutException($"{ex.Message} Muestra: «{SafeSectorInfo()}».", ex);
        }
    }

    private bool HexLbaMatches(ulong lba)
    {
        var m = HexLbaRegex().Match(HexText);
        return m.Success && DigitsOnly(m.Groups[1].Value) == lba.ToString(CultureInfo.InvariantCulture);
    }

    [GeneratedRegex(@"── LBA (\d[\d\W]*?) · ")]
    private static partial Regex HexLbaRegex();

    private string SafeSectorInfo()
    {
        try
        {
            return SectorInfo;
        }
        catch (Exception ex) when (ex is ElementNotAvailableException or TimeoutException)
        {
            return "?";
        }
    }

    /// <summary>Abre el menú de primer nivel indicado y devuelve el elemento de submenú pedido.</summary>
    public AutomationElement OpenMenuItem(string menuId, string itemId)
    {
        var menu = ById(menuId);
        ((ExpandCollapsePattern)menu.GetCurrentPattern(ExpandCollapsePattern.Pattern)).Expand();
        var cond = new PropertyCondition(AutomationElement.AutomationIdProperty, itemId);
        return WaitFor(() => menu.FindFirst(TreeScope.Descendants, cond), $"el elemento de menú «{itemId}»");
    }

    public void CloseMenu(string menuId)
    {
        var p = (ExpandCollapsePattern)ById(menuId).GetCurrentPattern(ExpandCollapsePattern.Pattern);
        if (p.Current.ExpandCollapseState != ExpandCollapseState.Collapsed)
        {
            p.Collapse();
        }
    }

    // ---------- Cierre ----------

    /// <summary>
    /// Cierra la ventana como un usuario y espera a que el proceso termine. Devuelve <c>true</c> si terminó
    /// por sí solo con código 0 dentro del tiempo límite.
    /// </summary>
    public bool CloseGracefully()
    {
        if (!_process.HasExited)
        {
            // WM_CLOSE, como el botón «Cerrar» de la ventana. WindowPattern.Close se ignora si antes se abrió
            // un menú por UI Automation, así que solo se usa como alternativa.
            _process.Refresh();
            if (!_process.CloseMainWindow())
            {
                try
                {
                    ((WindowPattern)Window.GetCurrentPattern(WindowPattern.Pattern)).Close();
                }
                catch (Exception ex) when (ex is ElementNotAvailableException or InvalidOperationException)
                {
                    // La ventana ya no existe; si el proceso sigue vivo se mata en Dispose.
                }
            }
            if (!_process.WaitForExit(10_000))
            {
                return false;
            }
        }
        return _process.ExitCode == 0;
    }

    public void Dispose()
    {
        try
        {
            CloseGracefully();
        }
        catch (Exception ex) when (ex is InvalidOperationException or System.ComponentModel.Win32Exception)
        {
            // Se mata a continuación.
        }
        finally
        {
            KillQuietly(_process);
            _process.Dispose();
        }
    }

    private static void KillQuietly(Process p)
    {
        try
        {
            if (!p.HasExited)
            {
                p.Kill(entireProcessTree: true);
                p.WaitForExit(10_000);
            }
        }
        catch (Exception ex) when (ex is InvalidOperationException or System.ComponentModel.Win32Exception)
        {
            // Ya terminó o no se puede acceder: no hay nada más que hacer.
        }
    }
}
