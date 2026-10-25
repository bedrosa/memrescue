using System.IO;
using System.Windows;
using MemRescue.Core.Interop;
using Microsoft.Win32;

namespace MemRescue;

/// <summary>
/// Elección explícita de la cadena de padres de un disco diferencial VHD/VHDX (ADR-018, addendum). Muestra lo que el
/// diferencial dice de su padre (solo como orientación: nunca se abre esa ruta) y deja elegir los ficheros, del padre
/// inmediato al disco base, con un diálogo de ficheros o escribiendo la ruta. Abrir exige una confirmación explícita.
/// </summary>
public partial class ParentChainDialog : Window
{
    private readonly List<string> _parents = [];

    /// <summary>Indica si el último padre añadido también es diferencial (la cadena no está completa).</summary>
    private bool _lastIsDifferencing = true;

    public ParentChainDialog(string childPath, IReadOnlyList<ExternalHint> hints)
    {
        InitializeComponent();
        IntroText.Text = $"«{Path.GetFileName(childPath)}» es un disco diferencial: los sectores que no ha escrito están en su " +
            "disco padre. Por seguridad MemRescue nunca abre el padre por su cuenta: elija usted el fichero padre (y, si " +
            "ese también es diferencial, el suyo, hasta el disco base). MemRescue comprobará que cada uno corresponde.";
        HintsBox.Text = FormatHints(hints);
        UpdateState();
    }

    /// <summary>Padres elegidos, del inmediato al base (solo tras <c>DialogResult = true</c>).</summary>
    public IReadOnlyList<string> Parents => _parents;

    /// <summary>El usuario pidió abrir aunque el identificador del padre no coincida.</summary>
    public bool IgnoreLinkMismatch => IgnoreMismatchCheck.IsChecked == true;

    internal static string FormatHints(IReadOnlyList<ExternalHint> hints) =>
        string.Join(Environment.NewLine, hints.Select(h => $"{h.Key}: {h.Value}"));

    private void Browse_Click(object sender, RoutedEventArgs e)
    {
        var dlg = new OpenFileDialog
        {
            Title = "Elegir el disco padre (se abrirá en solo lectura)",
            Filter = "Discos virtuales (*.vhd;*.vhdx;*.avhd;*.avhdx)|*.vhd;*.vhdx;*.avhd;*.avhdx|Todos los ficheros (*.*)|*.*",
            CheckFileExists = true,
        };
        if (dlg.ShowDialog(this) == true)
        {
            ParentPathBox.Text = dlg.FileName;
            AddPath(dlg.FileName);
        }
    }

    private void Add_Click(object sender, RoutedEventArgs e) => AddPath(ParentPathBox.Text.Trim());

    /// <summary>
    /// Añade un padre a la cadena. Solo se leen sus propias indicaciones (para saber si también es diferencial);
    /// la apertura y la validación del vínculo las hace el motor al pulsar «Abrir».
    /// </summary>
    private void AddPath(string path)
    {
        if (path.Length == 0)
        {
            StatusText.Text = "Escriba o elija la ruta del fichero padre.";
            return;
        }
        string full;
        try
        {
            full = Path.GetFullPath(path);
        }
        catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
        {
            StatusText.Text = $"Ruta no válida: {ex.Message}";
            return;
        }
        if (_parents.Contains(full, StringComparer.OrdinalIgnoreCase))
        {
            StatusText.Text = "Ese fichero ya está en la cadena.";
            return;
        }
        if (!_lastIsDifferencing && _parents.Count > 0)
        {
            StatusText.Text = "El último padre ya es un disco base: la cadena está completa.";
            return;
        }
        IReadOnlyList<ExternalHint> hints;
        try
        {
            hints = Engine.ExternalHints(full);
        }
        catch (EngineException ex)
        {
            StatusText.Text = $"No se puede usar ese fichero: {ex.Message}";
            return;
        }
        _parents.Add(full);
        _lastIsDifferencing = hints.Count > 0;
        var label = $"{_parents.Count}. {full}";
        if (_lastIsDifferencing)
        {
            label += $"  (también diferencial; su padre: {hints.FirstOrDefault(h => h.Key is "parent_name" or "relative_path")?.Value ?? "ver indicaciones"})";
        }
        ChainList.Items.Add(label);
        ParentPathBox.Clear();
        UpdateState();
    }

    private void Remove_Click(object sender, RoutedEventArgs e)
    {
        if (_parents.Count == 0)
        {
            return;
        }
        _parents.RemoveAt(_parents.Count - 1);
        ChainList.Items.RemoveAt(ChainList.Items.Count - 1);
        _lastIsDifferencing = true;
        if (_parents.Count > 0)
        {
            try
            {
                _lastIsDifferencing = Engine.ExternalHints(_parents[^1]).Count > 0;
            }
            catch (EngineException)
            {
                // Se volverá a comprobar al abrir.
            }
        }
        UpdateState();
    }

    private void Confirm_Changed(object sender, RoutedEventArgs e) => UpdateState();

    private void UpdateState()
    {
        var complete = _parents.Count > 0 && !_lastIsDifferencing;
        if (_parents.Count == 0)
        {
            StatusText.Text = "Añada el fichero padre inmediato.";
        }
        else if (!complete)
        {
            StatusText.Text = "El último padre también es diferencial: añada su padre (la cadena está incompleta).";
        }
        else
        {
            StatusText.Text = $"Cadena completa: {_parents.Count} {(_parents.Count == 1 ? "padre" : "padres")}; el último es un disco base.";
        }
        OpenButton.IsEnabled = complete && ConfirmCheck.IsChecked == true;
    }

    private void Open_Click(object sender, RoutedEventArgs e)
    {
        if (OpenButton.IsEnabled)
        {
            DialogResult = true;
        }
    }
}
