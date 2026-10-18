using System.Collections.ObjectModel;
using System.Globalization;
using System.IO;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Automation;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using MemRescue.Core.Interop;
using Microsoft.Win32;

namespace MemRescue;

/// <summary>Origen abierto en la sesión (imagen, contenedor, imagen dividida o rango), en solo lectura.</summary>
public sealed class SourceEntry(Source source, ImageFormat detected, bool forcedRaw, string? label = null, bool tolerant = false) : IDisposable
{
    public Source Source { get; } = source;

    /// <summary>Contenedor abierto en modo tolerante (bloques con entradas de asignación dañadas descartados).</summary>
    public bool Tolerant { get; } = tolerant;

    public ImageInfo Info => Source.Info;

    public ImageFormat Detected { get; } = detected;

    public bool ForcedRaw { get; } = forcedRaw;

    /// <summary>Primer sector mostrado en el visor.</summary>
    public ulong CurrentLba { get; set; }

    public string FileName => label ?? Path.GetFileName(Info.Path);

    public string Summary => $"{MainWindow.FormatSize(Info.SizeBytes)} · {Info.SectorCount:N0} × {Info.SectorSize} B"
        + (Tolerant ? " · MODO TOLERANTE" : "");

    /// <summary>Tabla de particiones leída (se lee al seleccionar el origen por primera vez).</summary>
    public PartitionTable? Partitions { get; set; }

    public void Dispose()
    {
        Partitions?.Dispose();
        Source.Dispose();
    }
}

/// <summary>Fila de la lista de particiones (textos ya formateados para la vista y UI Automation).</summary>
public sealed class PartitionRow(PartitionEntry entry, uint sectorSize)
{
    public PartitionEntry Entry { get; } = entry;

    public int Number => Entry.Number;

    public string Origin => Entry.OriginLabel;

    public string ActiveText => Entry.Active ? "sí" : "";

    public string StartText => Entry.StartLba.ToString("N0", CultureInfo.CurrentCulture);

    public string EndText => Entry.EndLba.ToString("N0", CultureInfo.CurrentCulture);

    public string SizeText => MainWindow.FormatSize(Entry.SectorCount * sectorSize);

    public string Type => Entry.TypeLabel;

    /// <summary>Nombre GPT (vacío en MBR).</summary>
    public string Name => Entry.Name;

    public string ConfidenceText => Entry.Confidence.Label();

    public string AutomationId => "Partition" + Entry.Number.ToString(CultureInfo.InvariantCulture);

    public string AutomationName =>
        $"Partición {Entry.Number}: {Entry.OriginLabel}, LBA {Entry.StartLba}–{Entry.EndLba}, {Entry.SectorCount} sectores, {Entry.TypeLabel}, " +
        (Entry.Name.Length > 0 ? $"«{Entry.Name}», " : "") +
        $"{(Entry.Active ? "activa, " : "")}confianza {Entry.Confidence.Label()}";

    public string Details => Entry.Anomalies.Count == 0
        ? AutomationName
        : AutomationName + "\n" + string.Join("\n", Entry.Anomalies.Select(a => a.ToString()));
}

public partial class MainWindow : Window
{
    private readonly ObservableCollection<SourceEntry> _sources = [];
    private CancellationTokenSource? _hashCts;
    private CancellationTokenSource? _verifyCts;
    private int _pageRequest;
    private bool _syncingScroll;

    /// <summary>Preferencias del usuario (patrones de relleno). Nunca se guardan junto a las imágenes.</summary>
    private readonly SettingsStore _settingsStore = SettingsStore.ForCurrentUser();
    private UserSettings _settings;

    /// <summary>Texto plano del volcado mostrado (copiar al portapapeles, autodiagnóstico).</summary>
    private string _hexText = "";

    public MainWindow()
    {
        InitializeComponent();
        SourcesList.ItemsSource = _sources;
        EngineStatusText.Text = $"Motor {Engine.EngineVersion} · ABI {Engine.AbiVersion}";
        Log($"Motor cargado: versión {Engine.EngineVersion}, ABI {Engine.AbiVersion}.");
        _settings = _settingsStore.Load(out var settingsWarning);
        if (settingsWarning is not null)
        {
            Log($"Aviso: {settingsWarning} ({_settingsStore.Path})");
        }
        Log($"Patrones de relleno: ilegibles {_settings.BadFill.Describe()}, saltados {_settings.SkipFill.Describe()}.");
        BuildLegend();
        InputBindings.Add(new KeyBinding(ApplicationCommands.Close, Key.F4, ModifierKeys.Control));
        PreviewKeyDown += Window_PreviewKeyDown;
        ContentRendered += OnContentRendered;
        SystemParameters.StaticPropertyChanged += SystemParameters_Changed;
        Closed += (_, _) =>
        {
            SystemParameters.StaticPropertyChanged -= SystemParameters_Changed;
            foreach (var s in _sources)
            {
                s.Dispose();
            }
        };
    }

    private SourceEntry? Selected => SourcesList.SelectedItem as SourceEntry;

    /// <summary>Tamaño de sector elegido para abrir; 0 = automático (nativo del formato, 512 en RAW).</summary>
    private uint SelectedSectorSize =>
        uint.TryParse(((ComboBoxItem)SectorSizeBox.SelectedItem).Content.ToString(), NumberStyles.None, CultureInfo.InvariantCulture, out var v)
            ? v
            : 0;

    private uint PageSectors =>
        uint.Parse(((ComboBoxItem)PageSizeBox.SelectedItem).Content.ToString()!, CultureInfo.InvariantCulture);

    internal static string FormatSize(ulong bytes)
    {
        string[] units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
        double v = bytes;
        var i = 0;
        while (v >= 1024 && i < units.Length - 1)
        {
            v /= 1024;
            i++;
        }
        return i == 0 ? $"{bytes} B" : $"{v:0.##} {units[i]}";
    }

    private void Log(string message)
    {
        LogList.Items.Add($"{DateTime.Now:HH:mm:ss}  {message}");
        LogList.ScrollIntoView(LogList.Items[^1]);
    }

    private void ShowError(string title, EngineException ex)
    {
        Log($"{title}: [{ex.Status}] {ex.Message}");
        var hint = ex.Status switch
        {
            EngineStatus.Corrupt => "\n\nLa estructura del contenedor está dañada o es incoherente; no se interpreta para no mostrar datos erróneos.",
            EngineStatus.ExternalReference => "\n\nEl contenedor depende de otro fichero (disco padre o extensión). Por seguridad MemRescue no abre ficheros referenciados automáticamente.",
            EngineStatus.UnsupportedFeature => "\n\nEsta variante del formato todavía no está soportada.",
            _ => "",
        };
        MessageBox.Show(this, ex.Message + hint, title, MessageBoxButton.OK, MessageBoxImage.Warning);
    }

    // ---------- Abrir / cerrar ----------

    private async void OpenImage_Executed(object sender, ExecutedRoutedEventArgs e)
    {
        var dlg = new OpenFileDialog
        {
            Title = "Abrir imagen de disco (solo lectura)",
            Filter = "Imágenes de disco (*.img;*.ima;*.bin;*.raw;*.dd;*.iso;*.vhd;*.vhdx;*.vmdk;*.001;*.000)|*.img;*.ima;*.bin;*.raw;*.dd;*.iso;*.vhd;*.vhdx;*.vmdk;*.001;*.000|Todos los ficheros (*.*)|*.*",
            CheckFileExists = true,
        };
        if (dlg.ShowDialog(this) == true)
        {
            await OpenImageAsync(dlg.FileName, SelectedSectorSize, interactive: true);
        }
    }

    /// <summary>
    /// Abre un origen. Los contenedores soportados (VHD, VHDX, VMDK monolithicSparse) se interpretan;
    /// si el fichero es el primer segmento de una imagen dividida se ofrece abrir el conjunto; si
    /// parece un contenedor sin lector, se pregunta antes de tratarlo como RAW.
    /// </summary>
    internal async Task<SourceEntry?> OpenImageAsync(string path, uint sectorSize, bool interactive)
    {
        try
        {
            StatusText.Text = "Abriendo imagen…";
            var format = await Task.Run(() => Engine.Probe(path));
            var forceRaw = false;
            var split = false;
            if (format == ImageFormat.Raw && interactive && LooksLikeFirstSegment(path))
            {
                IReadOnlyList<string> segments = [];
                try
                {
                    segments = await Task.Run(() => Engine.DiscoverSplit(path));
                }
                catch (EngineException ex)
                {
                    Log($"No se pudo enumerar la imagen dividida: [{ex.Status}] {ex.Message}");
                }
                if (segments.Count > 1)
                {
                    var list = string.Join("\n", segments.Take(12).Select(Path.GetFileName));
                    if (segments.Count > 12)
                    {
                        list += $"\n… y {segments.Count - 12} más";
                    }
                    var answer = MessageBox.Show(this,
                        $"«{Path.GetFileName(path)}» parece el primer segmento de una imagen dividida en {segments.Count} ficheros:\n\n{list}\n\n" +
                        "¿Abrir el conjunto como un único disco (solo lectura)? Si responde No, se abrirá solo este fichero.",
                        "Imagen dividida", MessageBoxButton.YesNoCancel, MessageBoxImage.Question, MessageBoxResult.Yes);
                    if (answer == MessageBoxResult.Cancel)
                    {
                        return null;
                    }
                    split = answer == MessageBoxResult.Yes;
                }
            }
            else if (!IsReadable(format))
            {
                Log($"Formato detectado en {Path.GetFileName(path)}: {format} (sin lector todavía).");
                if (!interactive)
                {
                    return null;
                }
                var answer = MessageBox.Show(this,
                    $"El fichero parece un contenedor {format}. MemRescue todavía no interpreta este formato.\n\n" +
                    "¿Abrirlo de todas formas como volcado RAW sector a sector? Solo es correcto si sabe que el fichero es un volcado sin cabeceras.",
                    "Formato no soportado", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No);
                if (answer != MessageBoxResult.Yes)
                {
                    return null;
                }
                forceRaw = true;
            }
            Source source;
            var tolerant = false;
            try
            {
                source = await Task.Run(() => Engine.OpenSource(path, sectorSize, forceRaw, split));
            }
            catch (EngineException ex) when (ex.Status == EngineStatus.Corrupt && !forceRaw && !split
                && format is ImageFormat.Vhd or ImageFormat.Vhdx or ImageFormat.Vmdk)
            {
                if (await OfferTolerantAsync(path, sectorSize, ex, interactive) is not { } t)
                {
                    return null;
                }
                source = t;
                tolerant = true;
            }
            var entry = new SourceEntry(source, format, forceRaw, tolerant: tolerant);
            AddSource(entry);
            Log($"Origen abierto en solo lectura: {path}, {source.Description} ({entry.Info.SizeBytes:N0} bytes, {entry.Info.SectorCount:N0} sectores de {entry.Info.SectorSize} B).");
            if (entry.Info.TrailingBytes > 0)
            {
                Log($"Aviso: {entry.Info.TrailingBytes} bytes finales no completan un sector de {entry.Info.SectorSize} B.");
            }
            return entry;
        }
        catch (EngineException ex)
        {
            if (interactive)
            {
                ShowError("No se pudo abrir la imagen", ex);
            }
            else
            {
                Log($"No se pudo abrir {path}: [{ex.Status}] {ex.Message}");
            }
            return null;
        }
        finally
        {
            StatusText.Text = "Listo";
        }
    }

    /// <summary>
    /// La apertura estricta de un contenedor falló con <see cref="EngineStatus.Corrupt"/>. Se evalúa
    /// en solo lectura si el daño se limita a las estructuras de asignación (el modo tolerante solo
    /// abre la imagen en ese caso; con cabeceras o metadatos críticos dañados vuelve a fallar) y, si
    /// es así, se pide confirmación explícita antes de abrirla en modo tolerante. Si el usuario no
    /// confirma, el origen evaluado se cierra sin añadirse a la sesión.
    /// </summary>
    private async Task<Source?> OfferTolerantAsync(string path, uint sectorSize, EngineException strictError, bool interactive)
    {
        Source candidate;
        try
        {
            candidate = await Task.Run(() => Engine.OpenSource(path, sectorSize, tolerant: true));
        }
        catch (EngineException)
        {
            throw strictError; // Daño en estructuras críticas: se informa el error original.
        }
        Log($"Apertura estricta rechazada ({strictError.Message}); el modo tolerante podría abrirla: {candidate.Description}.");
        var answer = interactive
            ? MessageBox.Show(this,
                $"«{Path.GetFileName(path)}» tiene estructuras de asignación dañadas:\n\n{strictError.Message}\n\n" +
                $"Puede abrirse en MODO TOLERANTE ({candidate.Description}).\n\n" +
                "En ese modo solo se leen los bloques cuya ubicación es inequívoca. Los bloques descartados se " +
                "muestran como ILEGIBLES (patrón de relleno), nunca como datos; los hashes de la imagen completa " +
                "fallarán y la comprobación los contará como no fiables. El fichero no se modifica.\n\n" +
                "¿Abrir la imagen en modo tolerante?",
                "Modo tolerante", MessageBoxButton.YesNo, MessageBoxImage.Warning, MessageBoxResult.No)
            : MessageBoxResult.No;
        if (answer != MessageBoxResult.Yes)
        {
            candidate.Dispose();
            Log($"No se abrió {path} en modo tolerante.");
            return null;
        }
        Log($"AVISO: {path} abierto en modo tolerante por decisión del usuario.");
        return candidate;
    }

    /// <summary>Formatos con lector en el motor (VMDK descriptor: el motor informa de la referencia externa).</summary>
    private static bool IsReadable(ImageFormat f) =>
        f is ImageFormat.Raw or ImageFormat.Vhd or ImageFormat.Vhdx or ImageFormat.Vmdk or ImageFormat.VmdkDescriptor;

    private static bool LooksLikeFirstSegment(string path)
    {
        var ext = Path.GetExtension(path);
        return ext.Length >= 4 && ext[1..].All(char.IsAsciiDigit) && ext[1..].TrimStart('0') is "" or "1";
    }

    private void AddSource(SourceEntry entry)
    {
        _sources.Add(entry);
        SourcesList.SelectedItem = entry;
        MainTabs.SelectedItem = SectorsTab;
    }

    private void OpenRange_Executed(object sender, ExecutedRoutedEventArgs e)
    {
        if (Selected is not { } s || s.Info.SectorCount == 0)
        {
            return;
        }
        var total = s.Info.SectorCount;
        var first = new InputNumberDialog("Abrir rango de sectores", "LBA inicial del rango:", 0, total - 1, s.CurrentLba) { Owner = this };
        if (first.ShowDialog() != true)
        {
            return;
        }
        var maxCount = total - first.Value;
        var second = new InputNumberDialog("Abrir rango de sectores",
            $"Número de sectores (1 – {maxCount:N0}) desde la LBA {first.Value:N0}:", 1, maxCount, maxCount) { Owner = this };
        if (second.ShowDialog() != true)
        {
            return;
        }
        _ = OpenRangeAsync(s, first.Value, second.Value);
    }

    /// <summary>Abre un rango de sectores del origen como un origen independiente.</summary>
    internal async Task<SourceEntry?> OpenRangeAsync(SourceEntry parent, ulong start, ulong count)
    {
        try
        {
            var src = await Task.Run(() => parent.Source.OpenRange(start, count));
            var entry = new SourceEntry(src, parent.Detected, parent.ForcedRaw,
                $"{parent.FileName} [LBA {start:N0}–{start + count - 1:N0}]", parent.Tolerant);
            AddSource(entry);
            Log($"Rango abierto como dispositivo: {src.Description} ({count:N0} sectores).");
            return entry;
        }
        catch (EngineException ex)
        {
            ShowError("No se pudo abrir el rango", ex);
            return null;
        }
    }

    private void HasSource_CanExecute(object sender, CanExecuteRoutedEventArgs e) => e.CanExecute = Selected is not null;

    private void CloseSource_Executed(object sender, ExecutedRoutedEventArgs e)
    {
        if (Selected is not { } s)
        {
            return;
        }
        _hashCts?.Cancel();
        _verifyCts?.Cancel();
        _sources.Remove(s);
        s.Dispose();
        Log($"Origen cerrado: {s.FileName}.");
        SourcesList.SelectedItem = _sources.LastOrDefault();
    }

    private void SourcesList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var s = Selected;
        var has = s is not null;
        NoSourceText.Visibility = has ? Visibility.Collapsed : Visibility.Visible;
        PropsGrid.Visibility = has ? Visibility.Visible : Visibility.Collapsed;
        NoSourceSectorsText.Visibility = NoSourceText.Visibility;
        HexView.Visibility = PropsGrid.Visibility;
        CommandManager.InvalidateRequerySuggested();
        LbaScrollBar.Visibility = PropsGrid.Visibility;
        if (s is null)
        {
            ClearPartitionsView("Abra una imagen de disco para ver su tabla de particiones.");
            SetHexMessage("");
            SectorStatesPanel.Children.Clear();
            SectorInfoText.Text = "";
            PositionText.Text = "";
            UntrustedBanner.Visibility = Visibility.Collapsed;
            TolerantBanner.Visibility = Visibility.Collapsed;
            return;
        }
        TolerantBanner.Visibility = s.Tolerant ? Visibility.Visible : Visibility.Collapsed;
        TolerantBannerText.Text = s.Tolerant
            ? $"MODO TOLERANTE: {s.Source.Description}. Los bloques descartados se muestran como ilegibles (patrón de relleno), nunca como datos."
            : "";
        PropPath.Text = s.Info.Path;
        PropFormat.Text = s.Source.Description;
        PropDetails.Text = s.Source.RangeStart is { } rs
            ? $"Rango del origen padre desde la LBA {rs:N0}"
            : s.ForcedRaw ? $"Leído como RAW por decisión del usuario (firmas de {s.Detected})" : $"Formato detectado: {s.Detected}";
        _syncingScroll = true;
        try
        {
            LbaScrollBar.Maximum = s.Info.SectorCount == 0 ? 0 : s.Info.SectorCount - 1;
            LbaScrollBar.Value = s.CurrentLba;
            LbaScrollBar.LargeChange = Math.Max(1, PageSectors);
            LbaScrollBar.ViewportSize = Math.Max(1, PageSectors);
        }
        finally
        {
            _syncingScroll = false;
        }
        PropSize.Text = $"{s.Info.SizeBytes:N0} bytes ({FormatSize(s.Info.SizeBytes)})";
        PropSector.Text = $"{s.Info.SectorSize} bytes";
        PropSectors.Text = $"{s.Info.SectorCount:N0} (LBA 0 – {(s.Info.SectorCount == 0 ? 0 : s.Info.SectorCount - 1):N0})";
        PropTrailing.Text = $"{s.Info.TrailingBytes}";
        _ = ShowSectorAsync(s.CurrentLba);
        _ = ShowPartitionsAsync(s);
    }

    // ---------- Particiones ----------

    private void ShowPartitionsTab_Click(object sender, RoutedEventArgs e) => MainTabs.SelectedItem = PartitionsTab;

    private void ClearPartitionsView(string message)
    {
        PartitionsList.ItemsSource = null;
        PartitionAnomaliesList.Items.Clear();
        PartitionSchemeText.Text = message;
        OpenPartitionButton.IsEnabled = false;
    }

    /// <summary>
    /// Muestra la tabla de particiones del origen (la lee la primera vez, en segundo plano). La lectura
    /// es tolerante: un sector de tabla ilegible aparece como anomalía, nunca se interpreta.
    /// </summary>
    internal async Task<PartitionTable?> ShowPartitionsAsync(SourceEntry s, bool reload = false)
    {
        if (reload && s.Partitions is { } old)
        {
            s.Partitions = null;
            old.Dispose();
        }
        if (s.Partitions is null)
        {
            ClearPartitionsView("Leyendo la tabla de particiones…");
            try
            {
                var table = await Task.Run(() => s.Source.ReadPartitions());
                if (s.Partitions is not null || !_sources.Contains(s))
                {
                    table.Dispose(); // otra lectura terminó antes o el origen se cerró
                }
                else
                {
                    s.Partitions = table;
                    Log($"Particiones de {s.FileName}: {table.Scheme.Label()}, {table.Partitions.Count} particiones, " +
                        $"confianza {table.Confidence.Label()}, {table.AllAnomalies.Count()} anomalías.");
                }
            }
            catch (EngineException ex)
            {
                Log($"No se pudo leer la tabla de particiones de {s.FileName}: [{ex.Status}] {ex.Message}");
                if (Selected == s)
                {
                    ClearPartitionsView($"No se pudo leer la tabla de particiones: {ex.Message}");
                }
                return null;
            }
        }
        if (Selected != s || s.Partitions is not { } t)
        {
            return s.Partitions;
        }
        var gpt = !t.HasGpt ? ""
            : t.GptSelected switch
            {
                GptCopyInUse.Primary => " · copia GPT: primaria",
                GptCopyInUse.Backup => " · COPIA GPT DE RESPALDO EN USO (la primaria no es utilizable)",
                _ => " · ninguna copia GPT es utilizable",
            };
        var shift = t.Shift != 0
            ? $" · IMAGEN DESPLAZADA {t.Shift} sectores (LBA disco = LBA imagen + {t.Shift}; las LBA mostradas son del disco)"
            : "";
        PartitionSchemeText.Text =
            $"Esquema: {t.Scheme.Label()} · {t.Partitions.Count} particiones · confianza del conjunto: {t.Confidence.Label()}{gpt}{shift}";
        PartitionsList.ItemsSource = t.Partitions.Select(p => new PartitionRow(p, t.SectorSize)).ToList();
        PartitionAnomaliesList.Items.Clear();
        foreach (var a in t.Anomalies)
        {
            PartitionAnomaliesList.Items.Add($"Tabla: {a}");
        }
        foreach (var p in t.Partitions)
        {
            foreach (var a in p.Anomalies)
            {
                PartitionAnomaliesList.Items.Add($"Partición {p.Number}: {a}");
            }
        }
        if (PartitionAnomaliesList.Items.Count == 0)
        {
            PartitionAnomaliesList.Items.Add("(ninguna)");
        }
        OpenPartitionButton.IsEnabled = false;
        return t;
    }

    private void PartitionsList_SelectionChanged(object sender, SelectionChangedEventArgs e) =>
        OpenPartitionButton.IsEnabled = PartitionsList.SelectedItem is PartitionRow;

    private void PartitionsList_MouseDoubleClick(object sender, MouseButtonEventArgs e) => OpenPartition_Click(sender, e);

    private void RefreshPartitions_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is { } s)
        {
            _ = ShowPartitionsAsync(s, reload: true);
        }
    }

    private void OpenPartition_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is { } s && PartitionsList.SelectedItem is PartitionRow row)
        {
            _ = OpenPartitionAsync(s, row.Entry);
        }
    }

    /// <summary>Abre una partición del origen como un origen independiente y la muestra en el visor.</summary>
    internal async Task<SourceEntry?> OpenPartitionAsync(SourceEntry parent, PartitionEntry p)
    {
        if (parent.Partitions is not { } table)
        {
            return null;
        }
        // Nunca se aplica en silencio el desplazamiento de una imagen: se pide confirmación.
        var applyShift = false;
        if (p.Origin == PartitionOrigin.Gpt && table.Shift != 0)
        {
            var image = (long)p.StartLba - table.Shift;
            var answer = MessageBox.Show(this,
                $"La imagen está desplazada {table.Shift} sectores respecto al disco original (LBA disco = LBA imagen + {table.Shift}).\n\n" +
                $"La partición {p.Number} declara la LBA {p.StartLba:N0} del disco, que corresponde a la LBA {image:N0} de la imagen.\n\n" +
                "¿Abrirla aplicando ese desplazamiento? La descripción del nuevo origen lo indicará.",
                "Imagen desplazada", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No);
            if (answer != MessageBoxResult.Yes)
            {
                Log($"No se abrió la partición {p.Number}: la imagen está desplazada y no se aplicó el desplazamiento.");
                return null;
            }
            applyShift = true;
        }
        try
        {
            var src = await Task.Run(() => table.OpenPartition(p.Index, applyShift));
            var entry = new SourceEntry(src, parent.Detected, parent.ForcedRaw,
                $"{parent.FileName} [partición {p.Number}: LBA {p.StartLba:N0}–{p.EndLba:N0}{(applyShift ? $" del disco, desplazamiento {table.Shift}" : "")}]", parent.Tolerant);
            AddSource(entry);
            Log($"Partición {p.Number} abierta como dispositivo: {src.Description} ({src.Info.SectorCount:N0} sectores, confianza {p.Confidence.Label()}).");
            return entry;
        }
        catch (EngineException ex)
        {
            ShowError("No se pudo abrir la partición", ex);
            return null;
        }
    }

    // ---------- Visor de sectores ----------

    /// <summary>Lee y muestra la página que empieza en <paramref name="lba"/>; ajusta al rango válido.</summary>
    internal async Task<bool> ShowSectorAsync(ulong lba)
    {
        if (Selected is not { } s)
        {
            return false;
        }
        var total = s.Info.SectorCount;
        if (total == 0)
        {
            SetHexMessage("(la imagen tiene menos de un sector completo)");
            SectorStatesPanel.Children.Clear();
            SectorInfoText.Text = "";
            return false;
        }
        lba = Math.Min(lba, total - 1);
        var count = (uint)Math.Min(PageSectors, total - lba);
        var request = Interlocked.Increment(ref _pageRequest);
        try
        {
            var fills = _settings;
            var page = await Task.Run(() => s.Source.ReadSectorsMapped(lba, count, retries: 1,
                badFill: fills.BadFill, skipFill: fills.SkipFill));
            if (request != _pageRequest || Selected != s)
            {
                return false; // llegó una petición más reciente
            }
            var ss = s.Info.SectorSize;
            var untrusted = RenderPage(lba, count, ss, page, fills);
            HexView.ScrollToHome();
            s.CurrentLba = lba;
            LbaBox.Text = lba.ToString(CultureInfo.InvariantCulture);
            _syncingScroll = true;
            try
            {
                LbaScrollBar.Value = lba;
            }
            finally
            {
                _syncingScroll = false;
            }
            var endLba = lba + count - 1;
            var states = count == 1 ? $" · {page.States[0].Label()}" : untrusted > 0 ? $" · {untrusted} NO FIABLES" : "";
            SectorInfoText.Text = (count == 1
                ? $"LBA {lba:N0} de {total - 1:N0} · desplazamiento {lba * ss:N0} (0x{lba * ss:X}) · sector de {ss} B"
                : $"LBA {lba:N0} – {endLba:N0} de {total - 1:N0} · desplazamiento {lba * ss:N0} (0x{lba * ss:X}) · {count} sectores de {ss} B") + states;
            UntrustedBanner.Visibility = untrusted > 0 ? Visibility.Visible : Visibility.Collapsed;
            UntrustedBannerText.Text = untrusted > 0
                ? $"{untrusted} de {count} sectores de esta página no se pudieron leer: su contenido es un patrón de relleno («{fills.BadFill}»/«{fills.SkipFill}»), no datos del origen."
                : "";
            PositionText.Text = $"LBA {lba:N0} / {total - 1:N0}";
            return true;
        }
        catch (EngineException ex)
        {
            Log($"Error leyendo LBA {lba}: [{ex.Status}] {ex.Message}");
            SetHexMessage($"No se pudo leer la LBA {lba:N0}:\n{ex.Message}");
            SectorStatesPanel.Children.Clear();
            return false;
        }
    }

    // ---------- Colores por estado y patrones de relleno ----------

    /// <summary>Ancho de página del volcado: cabe una línea de volcado completa sin ajuste.</summary>
    private const double HexPageWidth = 760;

    private static SolidColorBrush RgbBrush(uint rgb)
    {
        var b = new SolidColorBrush(Color.FromRgb((byte)(rgb >> 16), (byte)(rgb >> 8), (byte)rgb));
        b.Freeze();
        return b;
    }

    /// <summary>
    /// Fondo y texto de un estado. Con un tema de alto contraste se usan solo colores del sistema: los
    /// sectores no fiables, con los de resaltado; el resto, con los de la ventana (el texto los distingue).
    /// </summary>
    private static (Brush Background, Brush Foreground) StateBrushes(SectorState state)
    {
        if (SystemParameters.HighContrast)
        {
            return state.IsTrustworthy()
                ? (SystemColors.WindowBrush, SystemColors.WindowTextBrush)
                : (SystemColors.HighlightBrush, SystemColors.HighlightTextBrush);
        }
        var st = SectorStatePalette.For(state);
        return (RgbBrush(st.Background), RgbBrush(st.Foreground));
    }

    /// <summary>
    /// Sustituye el contenido del visor. Se reutiliza siempre el mismo <see cref="FlowDocument"/>: el
    /// proveedor de UI Automation del control (TextPattern) queda ligado al documento inicial.
    /// </summary>
    private void SetHexBlocks(IEnumerable<Block> blocks, string plainText)
    {
        var doc = HexView.Document;
        doc.PagePadding = new Thickness(2);
        doc.PageWidth = HexPageWidth;
        doc.Blocks.Clear();
        doc.Blocks.AddRange(blocks.ToList());
        _hexText = plainText;
    }

    /// <summary>Muestra un mensaje en el visor en lugar de un volcado.</summary>
    private void SetHexMessage(string message) => SetHexBlocks([new Paragraph(new Run(message))], message);

    /// <summary>
    /// Compone el volcado de la página: un bloque por sector con el color de su estado, la cabecera con el
    /// estado en texto y, en los no fiables, la advertencia de relleno con el patrón en uso y una barra
    /// lateral gruesa. Rellena también la fila de estados por sector. Devuelve los sectores no fiables.
    /// </summary>
    private int RenderPage(ulong lba, uint count, uint ss, MappedSectors page, UserSettings fills)
    {
        var blocks = new List<Block>();
        var text = new StringBuilder();
        var untrusted = 0;
        SectorStatesPanel.Children.Clear();
        SectorStatesPanel.Children.Add(new TextBlock
        {
            Text = count == 1 ? "Estado:" : "Estados:",
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 6, 2),
            Foreground = (Brush)FindResource("MutedBrush"),
        });
        for (uint i = 0; i < count; i++)
        {
            var sectorLba = lba + i;
            var offset = sectorLba * ss;
            var state = page.States[i];
            var trusted = state.IsTrustworthy();
            var (bg, fg) = StateBrushes(state);
            var para = new Paragraph
            {
                Background = bg,
                Foreground = fg,
                BorderBrush = fg,
                BorderThickness = new Thickness(trusted ? 1 : 6, 0, 0, 0),
                Padding = new Thickness(trusted ? 8 : 3, 2, 2, 2),
                Margin = new Thickness(0, 0, 0, 3),
            };
            var header = string.Create(CultureInfo.CurrentCulture, $"── LBA {sectorLba:N0} · desplazamiento {offset:N0} (0x{offset:X}) · {state.Label()} ──");
            para.Inlines.Add(new Bold(new Run(header)));
            text.Append(header).Append('\n');
            if (!trusted)
            {
                // Nunca se presenta el relleno como si fueran datos del origen.
                untrusted++;
                var pattern = state == SectorState.Skipped ? fills.SkipFill : fills.BadFill;
                var warning = $"!! SECTOR NO FIABLE: los bytes siguientes son un PATRÓN DE RELLENO {pattern.Describe()} generado por MemRescue, NO datos del origen.";
                para.Inlines.Add(new LineBreak());
                para.Inlines.Add(new Bold(new Run(warning)));
                text.Append(warning).Append('\n');
            }
            var dump = HexDump.Format(page.Data.AsSpan((int)(i * ss), (int)ss), offset);
            text.Append(dump);
            foreach (var line in dump.TrimEnd('\n').Split('\n'))
            {
                para.Inlines.Add(new LineBreak());
                para.Inlines.Add(new Run(line));
            }
            blocks.Add(para);
            SectorStatesPanel.Children.Add(StateChip(
                string.Create(CultureInfo.CurrentCulture, $"LBA {sectorLba:N0}: {SectorStatePalette.For(state).Tag}"),
                "SectorState" + sectorLba.ToString(CultureInfo.InvariantCulture), state));
        }
        SetHexBlocks(blocks, text.ToString());
        return untrusted;
    }

    /// <summary>Etiqueta con el color de un estado; el texto lo nombra (no depende solo del color).</summary>
    private static Border StateChip(string text, string automationId, SectorState state)
    {
        var (bg, fg) = StateBrushes(state);
        var label = new TextBlock
        {
            Text = text,
            Foreground = fg,
            FontWeight = state.IsTrustworthy() ? FontWeights.Normal : FontWeights.Bold,
        };
        AutomationProperties.SetAutomationId(label, automationId);
        AutomationProperties.SetHelpText(label, state.Label());
        return new Border
        {
            Background = bg,
            BorderBrush = fg,
            BorderThickness = new Thickness(state.IsTrustworthy() ? 1 : 2),
            Padding = new Thickness(4, 1, 4, 1),
            Margin = new Thickness(0, 0, 4, 2),
            Child = label,
        };
    }

    /// <summary>Leyenda de colores por estado y patrones de relleno en uso.</summary>
    private void BuildLegend()
    {
        while (StateLegend.Children.Count > 1)
        {
            StateLegend.Children.RemoveAt(1);
        }
        foreach (var state in SectorStatePalette.AllStates)
        {
            StateLegend.Children.Add(StateChip(SectorStatePalette.For(state).Tag, "Legend" + state, state));
        }
        var fills = new TextBlock
        {
            Text = $"Relleno (nunca son datos): ilegibles {_settings.BadFill.Describe()}, saltados {_settings.SkipFill.Describe()}",
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(6, 0, 0, 2),
            Foreground = (Brush)FindResource("MutedBrush"),
        };
        AutomationProperties.SetAutomationId(fills, "FillPatternsText");
        StateLegend.Children.Add(fills);
    }

    /// <summary>Al activar o desactivar un tema de alto contraste se recomponen la leyenda y la página.</summary>
    private void SystemParameters_Changed(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(SystemParameters.HighContrast))
        {
            Dispatcher.BeginInvoke(() =>
            {
                BuildLegend();
                if (Selected is { } s)
                {
                    _ = ShowSectorAsync(s.CurrentLba);
                }
            });
        }
    }

    /// <summary>
    /// Herramientas → Patrones de relleno. El cuadro se abre de forma asíncrona para que el menú (y los
    /// clientes de UI Automation) no queden bloqueados mientras está abierto.
    /// </summary>
    private void FillPatterns_Click(object sender, RoutedEventArgs e) =>
        Dispatcher.BeginInvoke(ShowFillPatternsDialog);

    private void ShowFillPatternsDialog()
    {
        var dlg = new FillPatternsDialog(_settings) { Owner = this };
        if (dlg.ShowDialog() != true || dlg.Result is not { } chosen)
        {
            return;
        }
        try
        {
            _settingsStore.Save(chosen);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException)
        {
            Log($"No se pudieron guardar las preferencias: {ex.Message}");
            MessageBox.Show(this, $"No se pudieron guardar las preferencias:\n\n{ex.Message}", "MemRescue",
                MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }
        _settings = chosen;
        Log($"Patrones de relleno guardados: ilegibles {chosen.BadFill.Describe()}, saltados {chosen.SkipFill.Describe()}.");
        BuildLegend();
        if (Selected is { } s)
        {
            _ = ShowSectorAsync(s.CurrentLba);
        }
    }


    private void Navigate(Func<SourceEntry, ulong> target)
    {
        if (Selected is { } s)
        {
            _ = ShowSectorAsync(target(s));
        }
    }

    private void PrevSector_Executed(object sender, ExecutedRoutedEventArgs e) =>
        Navigate(s => s.CurrentLba >= PageSectors ? s.CurrentLba - PageSectors : 0);

    private void NextSector_Executed(object sender, ExecutedRoutedEventArgs e) =>
        Navigate(s => s.CurrentLba + PageSectors < s.Info.SectorCount ? s.CurrentLba + PageSectors : s.CurrentLba);

    private void FirstSector_Executed(object sender, ExecutedRoutedEventArgs e) => Navigate(_ => 0);

    private void LastSector_Executed(object sender, ExecutedRoutedEventArgs e) =>
        Navigate(s => s.Info.SectorCount == 0 ? 0 : s.Info.SectorCount - 1);

    private void GoToSector_Executed(object sender, ExecutedRoutedEventArgs e)
    {
        if (Selected is not { } s || s.Info.SectorCount == 0)
        {
            return;
        }
        var dlg = new InputNumberDialog("Ir a sector", "Número de sector (LBA):", 0, s.Info.SectorCount - 1, s.CurrentLba) { Owner = this };
        if (dlg.ShowDialog() == true)
        {
            MainTabs.SelectedItem = SectorsTab;
            _ = ShowSectorAsync(dlg.Value);
        }
    }

    private void GoToOffset_Executed(object sender, ExecutedRoutedEventArgs e)
    {
        if (Selected is not { } s || s.Info.SectorCount == 0)
        {
            return;
        }
        var max = s.Info.SectorCount * s.Info.SectorSize - 1;
        var dlg = new InputNumberDialog("Ir a desplazamiento", "Desplazamiento en bytes desde el inicio del origen:", 0, max,
            s.CurrentLba * s.Info.SectorSize) { Owner = this };
        if (dlg.ShowDialog() == true)
        {
            var lba = dlg.Value / s.Info.SectorSize;
            Log($"Desplazamiento {dlg.Value:N0} (0x{dlg.Value:X}) → LBA {lba:N0}, byte {dlg.Value % s.Info.SectorSize} del sector.");
            MainTabs.SelectedItem = SectorsTab;
            _ = ShowSectorAsync(lba);
        }
    }

    private void GoLba_Click(object sender, RoutedEventArgs e) => GoToTypedLba();

    private void LbaBox_KeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Enter)
        {
            GoToTypedLba();
            e.Handled = true;
        }
    }

    private void GoToTypedLba()
    {
        if (Selected is not { } s)
        {
            return;
        }
        if (!HexDump.TryParseNumber(LbaBox.Text, out var lba) || lba >= s.Info.SectorCount)
        {
            StatusText.Text = $"LBA no válida (0 – {(s.Info.SectorCount == 0 ? 0 : s.Info.SectorCount - 1):N0})";
            System.Media.SystemSounds.Beep.Play();
            return;
        }
        StatusText.Text = "Listo";
        _ = ShowSectorAsync(lba);
    }

    private void PageSizeBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (IsLoaded && Selected is { } s)
        {
            LbaScrollBar.LargeChange = Math.Max(1, PageSectors);
            LbaScrollBar.ViewportSize = Math.Max(1, PageSectors);
            _ = ShowSectorAsync(s.CurrentLba);
        }
    }

    /// <summary>
    /// Barra de posición: recorre todo el origen sin cargarlo (solo se lee la página visible; las
    /// peticiones superadas por otras más recientes se descartan).
    /// </summary>
    private void LbaScrollBar_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_syncingScroll || Selected is not { } s || s.Info.SectorCount == 0)
        {
            return;
        }
        var lba = (ulong)Math.Clamp(Math.Round(e.NewValue), 0, s.Info.SectorCount - 1);
        if (lba != s.CurrentLba)
        {
            _ = ShowSectorAsync(lba);
        }
    }

    private void Window_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (Selected is null || MainTabs.SelectedItem != SectorsTab || Keyboard.FocusedElement is TextBox { Name: nameof(LbaBox) })
        {
            return;
        }
        if (e.Key == Key.PageUp && Keyboard.Modifiers == ModifierKeys.None)
        {
            Commands.PrevSector.Execute(null, this);
            e.Handled = true;
        }
        else if (e.Key == Key.PageDown && Keyboard.Modifiers == ModifierKeys.None)
        {
            Commands.NextSector.Execute(null, this);
            e.Handled = true;
        }
    }

    private void HexView_PreviewKeyDown(object sender, KeyEventArgs e) => Window_PreviewKeyDown(sender, e);

    private void CopyHex_Executed(object sender, ExecutedRoutedEventArgs e)
    {
        if (!string.IsNullOrEmpty(_hexText))
        {
            Clipboard.SetText(_hexText);
            Log("Volcado hexadecimal copiado al portapapeles.");
        }
    }

    // ---------- Comprobaciones: hashes ----------

    private void ShowChecksTab_Click(object sender, RoutedEventArgs e) => MainTabs.SelectedItem = ChecksTab;

    private HashAlgorithms SelectedAlgorithms()
    {
        var a = HashAlgorithms.None;
        if (AlgCrc32.IsChecked == true) a |= HashAlgorithms.Crc32;
        if (AlgMd5.IsChecked == true) a |= HashAlgorithms.Md5;
        if (AlgSha1.IsChecked == true) a |= HashAlgorithms.Sha1;
        if (AlgSha256.IsChecked == true) a |= HashAlgorithms.Sha256;
        return a;
    }

    private async void HashStart_Click(object sender, RoutedEventArgs e)
    {
        var s = Selected;
        if (s is null)
        {
            MessageBox.Show(this, "Primero abra y seleccione una imagen.", "MemRescue", MessageBoxButton.OK, MessageBoxImage.Information);
            return;
        }
        var algs = SelectedAlgorithms();
        if (algs == HashAlgorithms.None)
        {
            MessageBox.Show(this, "Seleccione al menos un algoritmo.", "MemRescue", MessageBoxButton.OK, MessageBoxImage.Information);
            return;
        }
        string? db = null;
        if (RecordJob.IsChecked == true)
        {
            var dir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "MemRescue");
            Directory.CreateDirectory(dir);
            db = Path.Combine(dir, "memrescue-work.mrdb");
        }
        await RunHashAsync(s, algs, db);
    }

    private void HashCancel_Click(object sender, RoutedEventArgs e)
    {
        _hashCts?.Cancel();
        Log("Cancelación del cálculo de hashes solicitada.");
    }

    internal async Task<IReadOnlyList<(HashAlgorithms Alg, string Hex)>?> RunHashAsync(SourceEntry s, HashAlgorithms algs, string? db)
    {
        HashStartButton.IsEnabled = false;
        HashCancelButton.IsEnabled = true;
        HashResults.Clear();
        HashProgress.Value = 0;
        _hashCts = new CancellationTokenSource();
        var started = DateTime.Now;
        Log($"Calculando {algs} de {s.FileName}{(db is null ? "" : $" (registrado en {db})")}…");
        try
        {
            using var task = s.Source.StartHash(algs, db);
            var progress = new Progress<TaskProgress>(p =>
            {
                HashProgress.Value = p.Fraction;
                HashProgressText.Text = $"{p.Fraction:P1}, {p.Done:N0} de {p.Total:N0} bytes";
            });
            var state = await task.WaitAsync(progress, _hashCts.Token);
            if (state == EngineTaskState.Cancelled)
            {
                Log("Cálculo de hashes cancelado.");
                HashProgressText.Text = "Cancelado";
                return null;
            }
            if (state == EngineTaskState.Failed)
            {
                task.GetResult(algs); // lanza EngineException con el motivo
            }
            var results = new List<(HashAlgorithms, string)>();
            foreach (var a in new[] { HashAlgorithms.Crc32, HashAlgorithms.Md5, HashAlgorithms.Sha1, HashAlgorithms.Sha256 })
            {
                if (algs.HasFlag(a))
                {
                    results.Add((a, task.GetResult(a)));
                }
            }
            HashResults.Text = string.Join(Environment.NewLine, results.Select(r => $"{r.Item1,-7} {r.Item2}"));
            Log($"Hashes calculados en {(DateTime.Now - started).TotalSeconds:0.00} s.");
            return results;
        }
        catch (EngineException ex)
        {
            Log($"Error en el cálculo de hashes: [{ex.Status}] {ex.Message}");
            HashProgressText.Text = "Error: " + ex.Message;
            return null;
        }
        finally
        {
            HashStartButton.IsEnabled = true;
            HashCancelButton.IsEnabled = false;
        }
    }

    // ---------- Comprobaciones: legibilidad ----------

    private async void VerifyStart_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is not { } s)
        {
            MessageBox.Show(this, "Primero abra y seleccione una imagen.", "MemRescue", MessageBoxButton.OK, MessageBoxImage.Information);
            return;
        }
        var retries = uint.Parse(((ComboBoxItem)RetriesBox.SelectedItem).Content.ToString()!, CultureInfo.InvariantCulture);
        var jump = ulong.Parse(((ComboBoxItem)JumpBox.SelectedItem).Content.ToString()!, CultureInfo.InvariantCulture);
        var ignoreAll = IgnoreErrorsBox.IsChecked == true;
        int[] skipCodes = [];
        if (!ignoreAll && !ErrorCodeList.TryParse(SkipCodesBox.Text, out skipCodes, out var codeError))
        {
            MessageBox.Show(this, codeError, "MemRescue", MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }
        await RunVerifyAsync(s, retries, jump, ReadBackBox.IsChecked == true && jump > 0, skipCodes, ignoreAll);
    }

    /// <summary>Con «ignorar todos los errores», la lista de códigos no tiene efecto.</summary>
    private void IgnoreErrorsBox_Changed(object sender, RoutedEventArgs e) =>
        SkipCodesBox.IsEnabled = IgnoreErrorsBox.IsChecked != true;

    private void VerifyCancel_Click(object sender, RoutedEventArgs e)
    {
        _verifyCts?.Cancel();
        Log("Cancelación de la comprobación de legibilidad solicitada.");
    }

    internal async Task<VerifyReport?> RunVerifyAsync(SourceEntry s, uint retries, ulong jump = 0, bool readBack = false,
        IReadOnlyCollection<int>? skipCodes = null, bool ignoreAll = false)
    {
        var policy = ignoreAll ? "ignorar todos los errores (sin reintentos)"
            : skipCodes is { Count: > 0 } ? $"sin reintentos para los códigos {ErrorCodeList.Format(skipCodes)}"
            : null;
        VerifyStartButton.IsEnabled = false;
        VerifyCancelButton.IsEnabled = true;
        VerifyResults.Clear();
        VerifyProgress.Value = 0;
        _verifyCts = new CancellationTokenSource();
        var started = DateTime.Now;
        Log($"Comprobando la legibilidad de {s.FileName} ({s.Info.SectorCount:N0} sectores, {retries} reintentos" +
            (jump > 0 ? $", salto de {jump} sectores{(readBack ? " con lectura hacia atrás" : "")}" : "") +
            (policy is null ? "" : $", política de errores: {policy}") + ")…");
        try
        {
            using var task = s.Source.StartVerify(retries: retries, jumpSectors: jump, readBack: readBack,
                skipErrorCodes: skipCodes, ignoreAllErrors: ignoreAll);
            var progress = new Progress<TaskProgress>(p =>
            {
                VerifyProgress.Value = p.Fraction;
                VerifyProgressText.Text = $"{p.Fraction:P1}, {p.Done:N0} de {p.Total:N0} sectores";
            });
            var state = await task.WaitAsync(progress, _verifyCts.Token);
            if (state == EngineTaskState.Cancelled)
            {
                Log("Comprobación cancelada.");
                VerifyProgressText.Text = "Cancelada";
                return null;
            }
            var r = task.GetReport();
            var sb = new StringBuilder();
            sb.AppendLine(CultureInfo.CurrentCulture, $"Sectores fiables: {r.SectorsTrustworthy:N0} de {r.SectorsRequested:N0}");
            sb.AppendLine(CultureInfo.CurrentCulture, $"  leídos             {r.SectorsRead:N0}");
            sb.AppendLine(CultureInfo.CurrentCulture, $"  recuperados        {r.SectorsRecovered:N0}");
            sb.AppendLine(CultureInfo.CurrentCulture, $"  dispersos (ceros)  {r.SectorsSparse:N0}");
            sb.AppendLine(CultureInfo.CurrentCulture, $"Sectores NO fiables (contenido = patrón de relleno):");
            sb.AppendLine(CultureInfo.CurrentCulture, $"  ilegibles          {r.SectorsUnreadable:N0}");
            sb.AppendLine(CultureInfo.CurrentCulture, $"  saltados           {r.SectorsSkipped:N0}");
            sb.AppendLine(CultureInfo.CurrentCulture, $"Lecturas repetidas:  {r.RetryReads:N0}");
            if (policy is not null)
            {
                sb.AppendLine(CultureInfo.CurrentCulture, $"Política de errores: {policy}");
                sb.AppendLine(CultureInfo.CurrentCulture, $"  errores omitidos (ilegibles sin reintentar)  {r.OmittedErrors:N0}");
            }
            foreach (var c in r.ErrorCodes)
            {
                sb.AppendLine(CultureInfo.CurrentCulture, $"  código de error {c.Code}: {c.Sectors:N0} sectores ilegibles");
            }
            foreach (var b in r.UntrustedRanges)
            {
                sb.AppendLine(CultureInfo.CurrentCulture, $"  LBA {b.Lba:N0} – {b.Lba + b.Count - 1:N0} ({b.Count:N0} sectores, {b.State.Label()})");
            }
            if (r.RangesTruncated)
            {
                sb.AppendLine("  (lista de tramos truncada; los recuentos son exactos)");
            }
            VerifyResults.Text = sb.ToString();
            Log($"Comprobación terminada en {(DateTime.Now - started).TotalSeconds:0.00} s: {r.SectorsUnreadable:N0} ilegibles, {r.SectorsSkipped:N0} saltados.");
            return r;
        }
        catch (EngineException ex)
        {
            Log($"Error en la comprobación: [{ex.Status}] {ex.Message}");
            VerifyProgressText.Text = "Error: " + ex.Message;
            return null;
        }
        finally
        {
            VerifyStartButton.IsEnabled = true;
            VerifyCancelButton.IsEnabled = false;
        }
    }

    // ---------- Otros ----------

    private void ToggleLog_Click(object sender, RoutedEventArgs e)
    {
        var show = LogMenuItem.IsChecked;
        LogRow.Height = show ? new GridLength(120) : new GridLength(0);
        LogSplitter.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        LogList.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
    }

    private void About_Click(object sender, RoutedEventArgs e) =>
        MessageBox.Show(this,
            $"MemRescue {typeof(MainWindow).Assembly.GetName().Version}\n" +
            $"Motor {Engine.EngineVersion} (ABI {Engine.AbiVersion})\n\n" +
            "Herramienta de recuperación de datos y edición de disco.\n" +
            "Versión de desarrollo (hito M2 en curso): imágenes RAW, VHD, VHDX, VMDK monolithicSparse y divididas en solo lectura, " +
            "rangos y particiones MBR/EBR como dispositivo, visor de sectores con estado de validez, hashes y comprobación de legibilidad.",
            "Acerca de MemRescue", MessageBoxButton.OK, MessageBoxImage.Information);

    private void Exit_Click(object sender, RoutedEventArgs e) => Close();

    // ---------- Autodiagnóstico (--selftest) ----------

    private async void OnContentRendered(object? sender, EventArgs e)
    {
        if (App.SelfTest is not { } st)
        {
            return;
        }
        var lines = new List<string>
        {
            "OK",
            $"window_title={Title}",
            $"engine_version={Engine.EngineVersion}",
            $"abi_version={Engine.AbiVersion}",
        };
        try
        {
            if (st.Image is not null)
            {
                var entry = await OpenImageAsync(st.Image, st.SectorSize, interactive: false);
                if (entry is null)
                {
                    lines[0] = "ERROR";
                    lines.Add("open_image=failed");
                }
                else
                {
                    lines.Add($"format={entry.Info.Format}");
                    lines.Add($"image_size={entry.Info.SizeBytes}");
                    lines.Add($"sector_size={entry.Info.SectorSize}");
                    lines.Add($"image_sectors={entry.Info.SectorCount}");
                    if (await ShowSectorAsync(st.ReadLba))
                    {
                        var sector = entry.Source.ReadSectors(st.ReadLba, 1);
                        lines.Add($"lba{st.ReadLba}_hex16={Convert.ToHexString(sector, 0, 16)}");
                        lines.Add($"viewer_first_line={_hexText.Split('\n')[1]}");
                    }
                    else
                    {
                        lines[0] = "ERROR";
                        lines.Add("read_lba=failed");
                    }
                    var r = await RunHashAsync(entry, HashAlgorithms.Sha256 | HashAlgorithms.Crc32, null);
                    if (r is null)
                    {
                        lines[0] = "ERROR";
                        lines.Add("hash=failed");
                    }
                    else
                    {
                        lines.AddRange(r.Select(x => $"{x.Alg.ToString().ToLowerInvariant()}={x.Hex}"));
                    }
                    var v = await RunVerifyAsync(entry, 1);
                    if (v is null)
                    {
                        lines[0] = "ERROR";
                        lines.Add("verify=failed");
                    }
                    else
                    {
                        lines.Add($"verify_ok={v.SectorsRead + v.SectorsRecovered}");
                        lines.Add($"verify_sparse={v.SectorsSparse}");
                        lines.Add($"verify_bad={v.SectorsUnreadable}");
                        lines.Add($"verify_skipped={v.SectorsSkipped}");
                    }
                    MainTabs.SelectedItem = SectorsTab;
                    await Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);
                }
            }
        }
        catch (Exception ex)
        {
            lines[0] = "ERROR";
            lines.Add($"exception={ex.GetType().Name}: {ex.Message}");
        }
        if (st.Screenshot is not null)
        {
            SaveScreenshot(st.Screenshot);
            lines.Add($"screenshot={st.Screenshot}");
        }
        await File.WriteAllLinesAsync(st.Output, lines);
        Close();
    }

    /// <summary>Renderiza el contenido de la ventana a PNG (independiente del DPI del llamador).</summary>
    private void SaveScreenshot(string path)
    {
        UpdateLayout();
        var root = (FrameworkElement)Content;
        var dpi = System.Windows.Media.VisualTreeHelper.GetDpi(this);
        var bmp = new System.Windows.Media.Imaging.RenderTargetBitmap(
            (int)Math.Ceiling(root.ActualWidth * dpi.DpiScaleX), (int)Math.Ceiling(root.ActualHeight * dpi.DpiScaleY),
            dpi.PixelsPerInchX, dpi.PixelsPerInchY, System.Windows.Media.PixelFormats.Pbgra32);
        bmp.Render(root);
        var enc = new System.Windows.Media.Imaging.PngBitmapEncoder();
        enc.Frames.Add(System.Windows.Media.Imaging.BitmapFrame.Create(bmp));
        using var fs = File.Create(path);
        enc.Save(fs);
    }
}
