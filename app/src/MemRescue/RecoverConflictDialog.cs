using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using MemRescue.Core.Interop;

namespace MemRescue;

/// <summary>
/// Pregunta de conflicto de la recuperación (política «Preguntar»): el nombre ya existe en el destino.
/// Cerrar el cuadro sin elegir equivale a renombrar (nunca se sustituye sin una elección explícita).
/// </summary>
public sealed class RecoverConflictDialog : Window
{
    public RecoverDecision Decision { get; private set; } = RecoverDecision.Rename;

    public bool ApplyToAll => _all.IsChecked == true;

    private readonly CheckBox _all;

    public RecoverConflictDialog(RecoverQuestion q)
    {
        Title = "Conflicto de nombre en el destino";
        SizeToContent = SizeToContent.WidthAndHeight;
        ResizeMode = ResizeMode.NoResize;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        AutomationProperties.SetAutomationId(this, "RecoverConflictDialog");
        var what = q.Directory ? "La carpeta" : "El fichero";
        var size = q.ExistingSize is { } s ? $" ({s.ToString("N0", CultureInfo.CurrentCulture)} bytes)" : "";
        var text = new TextBlock
        {
            Text = $"{what} «{q.Requested}» ya existe en el destino{size}:\n{q.Existing}\n\n" +
                   (q.Directory
                       ? "Puede renombrar la carpeta recuperada, omitirla con su contenido o reutilizar la existente."
                       : $"El fichero recuperado tiene {q.NewSize.ToString("N0", CultureInfo.CurrentCulture)} bytes" +
                         (q.Deleted ? " (versión BORRADA)" : "") + "."),
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 520,
            Margin = new Thickness(0, 0, 0, 10),
        };
        AutomationProperties.SetAutomationId(text, "ConflictText");
        _all = new CheckBox { Content = "Aplicar a todos los conflictos siguientes de este tipo", Margin = new Thickness(0, 0, 0, 10) };
        AutomationProperties.SetAutomationId(_all, "ConflictApplyAllBox");
        var buttons = new WrapPanel();
        void Add(string label, string id, RecoverDecision d)
        {
            var b = new Button { Content = label, Margin = new Thickness(0, 0, 6, 0), Padding = new Thickness(8, 2, 8, 2) };
            AutomationProperties.SetAutomationId(b, id);
            b.Click += (_, _) =>
            {
                Decision = d;
                DialogResult = true;
            };
            buttons.Children.Add(b);
        }
        Add("Renombrar", "ConflictRenameButton", RecoverDecision.Rename);
        Add("Omitir", "ConflictSkipButton", RecoverDecision.Skip);
        Add(q.Directory ? "Reutilizar la carpeta" : "Omitir si es idéntico", "ConflictSkipIdenticalButton", RecoverDecision.SkipIfIdentical);
        if (!q.Directory)
        {
            Add("Sustituir", "ConflictOverwriteButton", RecoverDecision.Overwrite);
        }
        Add("Cancelar la recuperación", "ConflictCancelButton", RecoverDecision.Cancel);
        Content = new StackPanel { Margin = new Thickness(12), Children = { text, _all, buttons } };
    }
}
