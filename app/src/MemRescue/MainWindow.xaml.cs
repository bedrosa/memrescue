using System.Collections.ObjectModel;
using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using MemRescue.Core.Interop;
using Microsoft.Win32;

namespace MemRescue;

/// <summary>Origen abierto en la sesión (en M0: ficheros de imagen).</summary>
public sealed record SourceEntry(ImageInfo Info)
{
    public string FileName => Path.GetFileName(Info.Path);

    public string Summary => $"{MainWindow.FormatSize(Info.SizeBytes)} · {Info.SectorCount:N0} sectores de {Info.SectorSize} B";
}

public partial class MainWindow : Window
{
    private readonly ObservableCollection<SourceEntry> _sources = [];
    private CancellationTokenSource? _hashCts;

    public MainWindow()
    {
        InitializeComponent();
        SourcesList.ItemsSource = _sources;
        EngineStatusText.Text = $"Motor {Engine.EngineVersion} · ABI {Engine.AbiVersion}";
        Log($"Motor cargado: versión {Engine.EngineVersion}, ABI {Engine.AbiVersion}.");
        ContentRendered += OnContentRendered;
    }

    private SourceEntry? Selected => SourcesList.SelectedItem as SourceEntry;

    private uint SelectedSectorSize =>
        uint.Parse(((ComboBoxItem)SectorSizeBox.SelectedItem).Content.ToString()!, CultureInfo.InvariantCulture);

    internal static string FormatSize(ulong bytes)
    {
        string[] units = ["B", "KiB", "MiB", "GiB", "TiB"];
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

    // ---------- Abrir imagen ----------

    private async void OpenImage_Executed(object sender, ExecutedRoutedEventArgs e)
    {
        var dlg = new OpenFileDialog
        {
            Title = "Abrir imagen de disco (solo lectura)",
            Filter = "Imágenes de disco (*.img;*.ima;*.bin;*.raw;*.dd)|*.img;*.ima;*.bin;*.raw;*.dd|Todos los ficheros (*.*)|*.*",
            CheckFileExists = true,
        };
        if (dlg.ShowDialog(this) == true)
        {
            await OpenImageAsync(dlg.FileName, SelectedSectorSize);
        }
    }

    private async Task<SourceEntry?> OpenImageAsync(string path, uint sectorSize)
    {
        try
        {
            StatusText.Text = "Abriendo imagen…";
            var info = await Task.Run(() => Engine.GetImageInfo(path, sectorSize));
            var entry = new SourceEntry(info);
            _sources.Add(entry);
            SourcesList.SelectedItem = entry;
            Log($"Imagen abierta en solo lectura: {path} ({info.SizeBytes:N0} bytes, {info.SectorCount:N0} sectores de {sectorSize} B).");
            if (info.TrailingBytes > 0)
            {
                Log($"Aviso: {info.TrailingBytes} bytes finales no completan un sector.");
            }
            return entry;
        }
        catch (EngineException ex)
        {
            Log($"Error al abrir {path}: [{ex.Status}] {ex.Message}");
            MessageBox.Show(this, ex.Message, "No se pudo abrir la imagen", MessageBoxButton.OK, MessageBoxImage.Warning);
            return null;
        }
        finally
        {
            StatusText.Text = "Listo";
        }
    }

    private void SourcesList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var s = Selected;
        NoSourceText.Visibility = s is null ? Visibility.Visible : Visibility.Collapsed;
        PropsGrid.Visibility = s is null ? Visibility.Collapsed : Visibility.Visible;
        if (s is null)
        {
            return;
        }
        PropPath.Text = s.Info.Path;
        PropSize.Text = $"{s.Info.SizeBytes:N0} bytes ({FormatSize(s.Info.SizeBytes)})";
        PropSector.Text = $"{s.Info.SectorSize} bytes";
        PropSectors.Text = $"{s.Info.SectorCount:N0}";
        PropTrailing.Text = $"{s.Info.TrailingBytes}";
    }

    // ---------- Hashes ----------

    private void ShowHashTab_Click(object sender, RoutedEventArgs e) => MainTabs.SelectedItem = HashTab;

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
        Log("Cancelación solicitada.");
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
            using var task = Engine.StartHash(s.Info.Path, algs, s.Info.SectorSize, db);
            var progress = new Progress<TaskProgress>(p =>
            {
                HashProgress.Value = p.Fraction;
                HashProgressText.Text = $"{p.Fraction:P1}, {p.Done:N0} de {p.Total:N0} bytes";
            });
            var state = await task.WaitAsync(progress, _hashCts.Token);
            if (state == EngineTaskState.Cancelled)
            {
                Log("Cálculo cancelado.");
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
            Log($"Error: [{ex.Status}] {ex.Message}");
            HashProgressText.Text = "Error";
            return null;
        }
        finally
        {
            HashStartButton.IsEnabled = true;
            HashCancelButton.IsEnabled = false;
        }
    }

    // ---------- Otros ----------

    private void ToggleLog_Click(object sender, RoutedEventArgs e)
    {
        var show = LogMenuItem.IsChecked;
        LogRow.Height = show ? new GridLength(130) : new GridLength(0);
        LogSplitter.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        LogList.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
    }

    private void About_Click(object sender, RoutedEventArgs e) =>
        MessageBox.Show(this,
            $"MemRescue {typeof(MainWindow).Assembly.GetName().Version}\n" +
            $"Motor {Engine.EngineVersion} (ABI {Engine.AbiVersion})\n\n" +
            "Herramienta de recuperación de datos y edición de disco.\n" +
            "Versión de desarrollo (hito M0): solo apertura de imágenes y cálculo de hashes, en solo lectura.",
            "Acerca de MemRescue", MessageBoxButton.OK, MessageBoxImage.Information);

    private void Exit_Click(object sender, RoutedEventArgs e) => Close();

    // ---------- Autodiagnóstico (--selftest) ----------

    private async void OnContentRendered(object? sender, EventArgs e)
    {
        if (App.SelfTestOutput is null)
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
            if (App.SelfTestImage is not null)
            {
                var entry = await OpenImageAsync(App.SelfTestImage, 512);
                if (entry is null)
                {
                    lines[0] = "ERROR";
                    lines.Add("open_image=failed");
                }
                else
                {
                    lines.Add($"image_size={entry.Info.SizeBytes}");
                    lines.Add($"image_sectors={entry.Info.SectorCount}");
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
                }
            }
        }
        catch (Exception ex)
        {
            lines[0] = "ERROR";
            lines.Add($"exception={ex.GetType().Name}: {ex.Message}");
        }
        if (App.ScreenshotPath is not null)
        {
            SaveScreenshot(App.ScreenshotPath);
            lines.Add($"screenshot={App.ScreenshotPath}");
        }
        await File.WriteAllLinesAsync(App.SelfTestOutput, lines);
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
