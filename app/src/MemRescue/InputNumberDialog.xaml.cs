using System.Windows;
using MemRescue.Core.Interop;

namespace MemRescue;

/// <summary>Pide un número (decimal, 0x… o …h) dentro de un rango cerrado [min, max].</summary>
public partial class InputNumberDialog : Window
{
    private readonly ulong _min;
    private readonly ulong _max;

    public InputNumberDialog(string title, string prompt, ulong min, ulong max, ulong initial)
    {
        InitializeComponent();
        Title = title;
        PromptText.Text = prompt;
        _min = min;
        _max = max;
        HintText.Text = $"Rango válido: {min:N0} – {max:N0} (0x{min:X} – 0x{max:X}). Admite decimal, 0x… o …h.";
        ValueBox.Text = initial.ToString(System.Globalization.CultureInfo.InvariantCulture);
        ValueBox.SelectAll();
    }

    public ulong Value { get; private set; }

    private void Ok_Click(object sender, RoutedEventArgs e)
    {
        if (!HexDump.TryParseNumber(ValueBox.Text, out var v))
        {
            ErrorText.Text = "Número no válido.";
            return;
        }
        if (v < _min || v > _max)
        {
            ErrorText.Text = "Fuera del rango válido.";
            return;
        }
        Value = v;
        DialogResult = true;
    }
}
