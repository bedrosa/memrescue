using System.Windows;
using MemRescue.Core.Interop;

namespace MemRescue;

/// <summary>
/// Edita los patrones de relleno de sectores ilegibles y saltados. Valida en vivo con la misma regla
/// que el motor (<see cref="FillPattern.Validate"/>); solo permite guardar un par admisible.
/// </summary>
public partial class FillPatternsDialog : Window
{
    public FillPatternsDialog(UserSettings current)
    {
        InitializeComponent();
        BadFillBox.Text = current.BadFill.ToDisplayText();
        SkipFillBox.Text = current.SkipFill.ToDisplayText();
        BadFillBox.SelectAll();
        Revalidate();
    }

    /// <summary>Preferencias aceptadas (solo tras <c>DialogResult = true</c>).</summary>
    public UserSettings? Result { get; private set; }

    private void Pattern_TextChanged(object sender, System.Windows.Controls.TextChangedEventArgs e)
    {
        if (IsInitialized)
        {
            Revalidate();
        }
    }

    /// <summary>Analiza ambos campos, muestra su interpretación y el primer error; devuelve el par si es admisible.</summary>
    private (FillPattern Bad, FillPattern Skip)? Revalidate()
    {
        var okBad = FillPattern.TryParse(BadFillBox.Text, out var bad, out var badError);
        var okSkip = FillPattern.TryParse(SkipFillBox.Text, out var skip, out var skipError);
        BadFillPreview.Text = okBad ? bad.Describe() : "";
        SkipFillPreview.Text = okSkip ? skip.Describe() : "";
        var error = !okBad ? $"Ilegibles: {badError}"
            : !okSkip ? $"Saltados: {skipError}"
            : FillPattern.Validate(bad, skip);
        ErrorText.Text = error ?? "";
        OkButton.IsEnabled = error is null;
        return error is null ? (bad, skip) : null;
    }

    private void Reset_Click(object sender, RoutedEventArgs e)
    {
        BadFillBox.Text = FillPattern.DefaultBad.ToDisplayText();
        SkipFillBox.Text = FillPattern.DefaultSkip.ToDisplayText();
        Revalidate();
    }

    private void Ok_Click(object sender, RoutedEventArgs e)
    {
        if (Revalidate() is not { } p)
        {
            return;
        }
        Result = new UserSettings { BadFill = p.Bad, SkipFill = p.Skip };
        DialogResult = true;
    }
}
