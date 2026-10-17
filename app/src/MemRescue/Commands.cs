using System.Windows.Input;

namespace MemRescue;

/// <summary>Comandos de la ventana principal con sus atajos de teclado.</summary>
public static class Commands
{
    public static readonly RoutedUICommand GoToSector = new("Ir a sector", nameof(GoToSector), typeof(Commands),
        [new KeyGesture(Key.G, ModifierKeys.Control)]);

    public static readonly RoutedUICommand GoToOffset = new("Ir a desplazamiento", nameof(GoToOffset), typeof(Commands),
        [new KeyGesture(Key.G, ModifierKeys.Control | ModifierKeys.Shift)]);

    // RePág/AvPág se gestionan en la ventana (sin modificadores no se admiten como KeyGesture en todos los casos).
    public static readonly RoutedUICommand PrevSector = new("Sector anterior", nameof(PrevSector), typeof(Commands));

    public static readonly RoutedUICommand NextSector = new("Sector siguiente", nameof(NextSector), typeof(Commands));

    public static readonly RoutedUICommand FirstSector = new("Primer sector", nameof(FirstSector), typeof(Commands),
        [new KeyGesture(Key.Home, ModifierKeys.Control)]);

    public static readonly RoutedUICommand LastSector = new("Último sector", nameof(LastSector), typeof(Commands),
        [new KeyGesture(Key.End, ModifierKeys.Control)]);

    public static readonly RoutedUICommand OpenRange = new("Abrir rango de sectores como dispositivo", nameof(OpenRange), typeof(Commands),
        [new KeyGesture(Key.R, ModifierKeys.Control | ModifierKeys.Shift)]);

    public static readonly RoutedUICommand CopyHex = new("Copiar volcado hexadecimal", nameof(CopyHex), typeof(Commands),
        [new KeyGesture(Key.C, ModifierKeys.Control | ModifierKeys.Shift)]);
}
