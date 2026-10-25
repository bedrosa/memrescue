using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using MemRescue.Core.Interop;
using Microsoft.Win32;

namespace MemRescue;

/// <summary>
/// Extensiones de un descriptor VMDK separado (incidencia 11, ADR-018). Muestra lo que declara el
/// descriptor (sin abrir nada) y pide al usuario el fichero de cada extensión necesaria: se sugiere
/// el del mismo directorio con el nombre exacto declarado, pero cada uno debe confirmarse por
/// separado (casilla) o elegirse con el selector de ficheros. No hay «aceptar todo».
/// </summary>
public partial class VmdkExtentsDialog : Window
{
    private sealed class Row(VmdkExtent extent, TextBox pathBox, CheckBox confirm)
    {
        public VmdkExtent Extent { get; } = extent;
        public TextBox PathBox { get; } = pathBox;
        public CheckBox Confirm { get; } = confirm;
    }

    private readonly List<Row> _rows = [];
    private readonly string _descriptorPath;

    public VmdkExtentsDialog(string descriptorPath, VmdkExtentList list)
    {
        InitializeComponent();
        _descriptorPath = descriptorPath;
        SummaryText.Text = $"{Path.GetFileName(descriptorPath)}: {list.Summary}, {list.VirtualSize:N0} bytes";
        if (!list.Openable)
        {
            IssueText.Text = "No se puede abrir: " + list.Issue;
            IssueText.Visibility = Visibility.Visible;
        }
        BuildRows(list);
        UpdateOpenButton(list.Openable);
        Openable = list.Openable;
    }

    private bool Openable { get; }

    /// <summary>Ficheros aprobados (nombre declarado → ruta), en el orden del descriptor.</summary>
    public IReadOnlyList<ApprovedExtent> Approved { get; private set; } = [];

    private void BuildRows(VmdkExtentList list)
    {
        string[] headers = ["#", "Acceso", "Tipo", "Sectores", "Nombre en el descriptor", "Fichero elegido", "", "Confirmado"];
        foreach (var _ in headers)
        {
            RowsGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        }
        RowsGrid.ColumnDefinitions[5].Width = new GridLength(320);
        RowsGrid.RowDefinitions.Add(new RowDefinition());
        for (var c = 0; c < headers.Length; c++)
        {
            Place(new TextBlock { Text = headers[c], FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 0, 10, 4) }, 0, c);
        }
        var dir = Path.GetDirectoryName(Path.GetFullPath(_descriptorPath)) ?? "";
        foreach (var e in list.Extents)
        {
            var r = RowsGrid.RowDefinitions.Count;
            RowsGrid.RowDefinitions.Add(new RowDefinition());
            var id = e.Index.ToString(CultureInfo.InvariantCulture);
            Place(Cell((e.Index + 1).ToString(CultureInfo.InvariantCulture)), r, 0);
            Place(Cell(e.AccessName), r, 1);
            Place(Cell(e.KindName), r, 2);
            Place(Cell(e.Sectors.ToString("N0", CultureInfo.CurrentCulture)), r, 3);
            var name = Cell(e.FileName.Length == 0 ? "-" : e.FileName);
            AutomationProperties.SetAutomationId(name, "ExtentName" + id);
            Place(name, r, 4);
            if (!e.NeedsFile || e.IssueKind != VmdkIssueKind.None)
            {
                var note = e.IssueKind != VmdkIssueKind.None
                    ? "PROBLEMA: " + e.Issue
                    : e.Access == VmdkExtentAccess.NoAccess
                        ? "No se abre: NOACCESS, se leerá como ILEGIBLE"
                        : "Sin fichero: se lee como ceros (disperso)";
                var t = Cell(note);
                t.TextWrapping = TextWrapping.Wrap;
                AutomationProperties.SetAutomationId(t, "ExtentNote" + id);
                Place(t, r, 5);
                Grid.SetColumnSpan(t, 3);
                continue;
            }
            // Sugerencia: el nombre exacto en el mismo directorio, si existe. No se abre.
            var suggested = Path.Combine(dir, e.FileName);
            var pathBox = new TextBox
            {
                IsReadOnly = true,
                Text = File.Exists(suggested) ? suggested : "",
                Margin = new Thickness(0, 2, 6, 2),
                FontFamily = new System.Windows.Media.FontFamily("Consolas"),
            };
            AutomationProperties.SetAutomationId(pathBox, "ExtentPath" + id);
            AutomationProperties.SetName(pathBox, "Fichero de la extensión " + (e.Index + 1).ToString(CultureInfo.InvariantCulture));
            var choose = new Button { Content = "Elegir…", Padding = new Thickness(6, 0, 6, 0), Margin = new Thickness(0, 2, 6, 2) };
            AutomationProperties.SetAutomationId(choose, "ExtentChoose" + id);
            var confirm = new CheckBox
            {
                Content = "Usar este fichero",
                VerticalAlignment = VerticalAlignment.Center,
                IsEnabled = pathBox.Text.Length > 0,
            };
            AutomationProperties.SetAutomationId(confirm, "ExtentConfirm" + id);
            var row = new Row(e, pathBox, confirm);
            _rows.Add(row);
            confirm.Checked += (_, _) => UpdateOpenButton(Openable);
            confirm.Unchecked += (_, _) => UpdateOpenButton(Openable);
            choose.Click += (_, _) => Choose(row, dir);
            Place(pathBox, r, 5);
            Place(choose, r, 6);
            Place(confirm, r, 7);
        }
    }

    private static TextBlock Cell(string text) =>
        new() { Text = text, Margin = new Thickness(0, 2, 10, 2), VerticalAlignment = VerticalAlignment.Center };

    private void Place(UIElement el, int row, int col)
    {
        Grid.SetRow(el, row);
        Grid.SetColumn(el, col);
        RowsGrid.Children.Add(el);
    }

    /// <summary>Elegir un fichero con el selector es una aprobación explícita de ese fichero.</summary>
    private void Choose(Row row, string dir)
    {
        var dlg = new OpenFileDialog
        {
            Title = $"Fichero de la extensión «{row.Extent.FileName}» (solo lectura)",
            InitialDirectory = dir,
            FileName = row.Extent.FileName,
            CheckFileExists = true,
            Filter = "Todos los ficheros (*.*)|*.*",
        };
        if (dlg.ShowDialog(this) == true)
        {
            row.PathBox.Text = dlg.FileName;
            row.Confirm.IsEnabled = true;
            row.Confirm.IsChecked = true;
        }
        UpdateOpenButton(Openable);
    }

    private void UpdateOpenButton(bool openable) =>
        OpenButton.IsEnabled = openable && _rows.All(r => r.Confirm.IsChecked == true && r.PathBox.Text.Length > 0);

    private void Open_Click(object sender, RoutedEventArgs e)
    {
        if (!OpenButton.IsEnabled)
        {
            return;
        }
        Approved = [.. _rows.Select(r => new ApprovedExtent(r.Extent.FileName, r.PathBox.Text))];
        DialogResult = true;
    }
}
