using System.Collections.ObjectModel;
using System.Globalization;
using System.IO;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using MemRescue.Core.Interop;
using Microsoft.Win32;

namespace MemRescue;

/// <summary>Origen abierto en la sesión (imagen, contenedor, imagen dividida o rango), en solo lectura.</summary>
public sealed class SourceEntry(Source source, ImageFormat detected, bool forcedRaw, string? label = null) : IDisposable
{
    public Source Source { get; } = source;

    public ImageInfo Info => Source.Info;

    public ImageFormat Detected { get; } = detected;

    public bool ForcedRaw { get; } = forcedRaw;

    /// <summary>Primer sector mostrado en el visor.</summary>
    public ulong CurrentLba { get; set; }

    public string FileName => label ?? Path.GetFileName(Info.Path);

    public string Summary => $"{MainWindow.FormatSize(Info.SizeBytes)} · {Info.SectorCount:N0} × {Info.SectorSize} B";

    public void Dispose() => Source.Dispose();
}

public partial class MainWindow : Window
{
    private readonly ObservableCollection<SourceEntry> _sources = [];
    private CancellationTokenSource? _hashCts;
    private CancellationTokenSource? _verifyCts;
    private int _pageRequest;
    private bool _syncingScroll;

    public MainWindow()
    {
        InitializeComponent();
        SourcesList.ItemsSource = _sources;
        EngineStatusText.Text = $"Motor {Engine.EngineVersion} · ABI {Engine.AbiVersion}";
        Log($"Motor cargado: versión {Engine.EngineVersion}, ABI {Engine.AbiVersion}.");
        InputBindings.Add(new KeyBinding(ApplicationCommands.Close, Key.F4, ModifierKeys.Control));
        PreviewKeyDown += Window_PreviewKeyDown;
        ContentRendered += OnContentRendered;
        Closed += (_, _) =>
        {
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
            var source = await Task.Run(() => Engine.OpenSource(path, sectorSize, forceRaw, split));
            var entry = new SourceEntry(source, format, forceRaw);
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
                $"{parent.FileName} [LBA {start:N0}–{start + count - 1:N0}]");
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
            HexView.Clear();
            SectorInfoText.Text = "";
            PositionText.Text = "";
            UntrustedBanner.Visibility = Visibility.Collapsed;
            return;
        }
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
            HexView.Text = "(la imagen tiene menos de un sector completo)";
            SectorInfoText.Text = "";
            return false;
        }
        lba = Math.Min(lba, total - 1);
        var count = (uint)Math.Min(PageSectors, total - lba);
        var request = Interlocked.Increment(ref _pageRequest);
        try
        {
            var page = await Task.Run(() => s.Source.ReadSectorsMapped(lba, count, retries: 1));
            if (request != _pageRequest || Selected != s)
            {
                return false; // llegó una petición más reciente
            }
            var ss = s.Info.SectorSize;
            var sb = new StringBuilder();
            var untrusted = 0;
            for (uint i = 0; i < count; i++)
            {
                var sectorLba = lba + i;
                var offset = sectorLba * ss;
                var state = page.States[i];
                sb.Append(CultureInfo.CurrentCulture, $"── LBA {sectorLba:N0} · desplazamiento {offset:N0} (0x{offset:X}) · {state.Label()} ──\n");
                if (!state.IsTrustworthy())
                {
                    // Nunca se presenta el relleno como si fueran datos del origen.
                    untrusted++;
                    sb.Append("!! SECTOR NO FIABLE: los bytes siguientes son un PATRÓN DE RELLENO generado por MemRescue, NO datos del origen.\n");
                }
                sb.Append(HexDump.Format(page.Data.AsSpan((int)(i * ss), (int)ss), offset));
            }
            HexView.Text = sb.ToString();
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
                ? $"{untrusted} de {count} sectores de esta página no se pudieron leer: su contenido es un patrón de relleno («BAD!»/«SKIP»), no datos del origen."
                : "";
            PositionText.Text = $"LBA {lba:N0} / {total - 1:N0}";
            return true;
        }
        catch (EngineException ex)
        {
            Log($"Error leyendo LBA {lba}: [{ex.Status}] {ex.Message}");
            HexView.Text = $"No se pudo leer la LBA {lba:N0}:\n{ex.Message}";
            return false;
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
        if (!string.IsNullOrEmpty(HexView.Text))
        {
            Clipboard.SetText(HexView.Text);
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
        await RunVerifyAsync(s, retries, jump, ReadBackBox.IsChecked == true && jump > 0);
    }

    private void VerifyCancel_Click(object sender, RoutedEventArgs e)
    {
        _verifyCts?.Cancel();
        Log("Cancelación de la comprobación de legibilidad solicitada.");
    }

    internal async Task<VerifyReport?> RunVerifyAsync(SourceEntry s, uint retries, ulong jump = 0, bool readBack = false)
    {
        VerifyStartButton.IsEnabled = false;
        VerifyCancelButton.IsEnabled = true;
        VerifyResults.Clear();
        VerifyProgress.Value = 0;
        _verifyCts = new CancellationTokenSource();
        var started = DateTime.Now;
        Log($"Comprobando la legibilidad de {s.FileName} ({s.Info.SectorCount:N0} sectores, {retries} reintentos" +
            (jump > 0 ? $", salto de {jump} sectores{(readBack ? " con lectura hacia atrás" : "")}" : "") + ")…");
        try
        {
            using var task = s.Source.StartVerify(retries: retries, jumpSectors: jump, readBack: readBack);
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
            "Versión de desarrollo (hito M1): imágenes RAW, VHD, VHDX, VMDK monolithicSparse y divididas en solo lectura, " +
            "rangos como dispositivo, visor de sectores con estado de validez, hashes y comprobación de legibilidad.",
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
                        lines.Add($"viewer_first_line={HexView.Text.Split('\n')[1]}");
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
