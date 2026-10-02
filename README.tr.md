<div align="center">

<img src="crates/headshell/icons/128x128.png" width="104" alt="headshell">

# headshell

### Müziğin nereden geldiği değişir. **Dinleme kimliğin sende kalır.**

<p align="center">
  Sağlayıcıların <i>üstünde</i> duran bir müzik katmanı: geçmişin, istatistiklerin ve<br>
  müzik zevkin bir şirketin sunucusunda değil, senin makinende yaşar.
</p>

[![Lisans](https://img.shields.io/badge/lisans-MIT%20%7C%20Apache--2.0-2f6feb?style=for-the-badge)](LICENSE-MIT)
[![Rust](https://img.shields.io/badge/rust-1.87%2B-e07b39?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/linux%20·%20macOS%20·%20windows-3c3833?style=for-the-badge)](#kurulum)
[![Durum](https://img.shields.io/badge/durum-v0.0.3--beta-ffb454?style=for-the-badge)](#kurulum)

**[Ne yapar](#ne-yapar)** · **[Kurulum](#kurulum)** · **[Nasıl kullanılır](#nasıl-kullanılır)** · **[Nereden çalar](#nereden-çalar)** · **[Gizlilik](#gizlilik)** · **[Yol haritası](#yol-haritası)**

<sub><a href="README.md">English</a> · <b>Türkçe</b></sub>

<br>

<img src="docs/sample-card.tr.svg" alt="headshell'ın ürettiği örnek Sleeve kartı" width="600">

<sub><i>Yılda bir kez değil, istediğin an. Hangi yıl istersen.</i></sub>

</div>

> **Türkçe kopya.** Kanonik metin İngilizcedir: [`README.md`](README.md).
> Bu kopya 2026-09-25 tarihli hâlidir; İngilizce metinle birlikte güncel
> tutulacağı garanti değildir (D-073).

---

## Yıllarca dinledin. Peki geçmişin kimin?

Bir akış servisinde on yıl geçirirsin; her çalma, her keşif, her gece 3'te
tekrar tekrar dinlediğin o şarkı kaydedilir. Sonra aboneliği bırakırsın ve
hepsi orada kalır. On yıllık dinleme kimliğin, taşıyamadığın bir hesap
ekranına dönüşür.

**`headshell` bu ilişkiyi ters çevirir.** Geçmişini kendi makinene indirir, tek
bir kanonik kimlik altında toplar, ve müziği nereden çalarsan çal — yerel
diskten, evindeki sunucudan, bir eklentiden — üstteki katmanı aynı tutar.
Adı da buradan geliyor: pikap kolu plağı seçmez, ne koyarsan onu okur.

|  | Akış servisi | `headshell` |
| :--- | :--- | :--- |
| **Geçmişin nerede** | Şirketin sunucusunda | Kendi diskinde, SQLite dosyasında |
| **Ne kadar geriye gider** | Abonelik sürdüğü kadar | Export'un kapsadığı kadar — ömür boyu |
| **Yıl sonu kartı** | Yılda bir kez, son 12 ay (Spotify Wrapped®) | İstediğin an, istediğin yıl, tüm zamanlar |
| **Müzik nereden gelir** | Tek katalog | Yerel dosya, Subsonic, Jellyfin, eklentiler |
| **Telemetri** | Var | Yok — kodda tek satırı bile yok |
| **Ağ** | Zorunlu | İsteğe bağlı; `--online` demedikçe kimseye sorulmaz |

---

## Ne yapar

### 📥 Geçmişini içeri alır — API'siz, paylaşımsız

Spotify'ın GDPR kapsamında verdiği veri export'unu okur. Şifre istemez, API
anahtarı istemez, hesabına bağlanmaz. Yasal olarak senin olan bir dosyayı
okur, o kadar.

```bash
headshell import spotify_verilerim.zip
```

### 🔗 Aynı parçanın bütün kopyalarını tek kimlikte toplar

Yerel FLAC'in, Jellyfin'deki kopyan ve SoundCloud'daki yükleme — üçü de aynı
şarkı. `headshell` bunları dört halkalı bir zincirle eşler: **ISRC →
MusicBrainz → bulanık eşleşme → AcoustID parmak izi.** Her eşleşme bir güven
skoru taşır, ve eşleşmeyenler **sayılır** — sessizce kaybolmaz.

### 📊 İstatistiklerin, istediğin an

```text
$ headshell stats --year 2024 --top 3

dönem: 2024
4128 çalma · 271.3 saat · 1163 parça · 402 sanatçı
kapsamda 4310 kayıt, 182 kısa çalma atlandı, 0 kayıt kapsam dışı, 37 kayıt kimliksiz

en çok dinlenen sanatçılar
   1. Pink Floyd                         312 çalma  28sa 41dk  47 parça
   2. Radiohead                          268 çalma  21sa 12dk  53 parça
   3. Daft Punk                          193 çalma  14sa 55dk  31 parça

en çok dinlenen parçalar
   1. Pink Floyd - Time                               38 çalma  4sa 21dk
   2. Radiohead - Weird Fishes/Arpeggi                29 çalma  2sa 34dk
   3. Daft Punk - Digital Love                        27 çalma  2sa 10dk
```

<sub>Sayılar örnek; biçim programın bastığının aynısı. Üçüncü satıra dikkat:
atlanan ve kimliklenemeyen kayıtlar <b>sayılarak</b> raporlanır — bu proje
"bakmadım" ile "bulamadım"ı ayrı tanılar sayar.</sub>

### 🎨 Paylaşılabilir Sleeve kartı

Kare (1080×1080) ya da hikâye (1080×1920), SVG ya da PNG. Aralığı beklemene
gerek yok, yılı sen seçersin.

```bash
headshell sleeve --year 2024 --format story --out 2024.png
```

### 🎧 Ve çalar

Yerel dosyaların, evindeki Subsonic/Navidrome/Jellyfin sunucun ve eklentiler
üzerinden. Kütüphane SQLite FTS5 ile indekslenir, arama anında döner.

```bash
headshell play "Pink Floyd" --all --tui
```

```text
┌ headshell ───────────────────────────────────────────────────────────────────┐
│ ▶ Pink Floyd - Time  (çalıyor)                                             │
└────────────────────────────────────────────────────────────────────────────┘
┌────────────────────────────────────────────────────────────────────────────┐
│████████████████████████████    2:45 / 6:53                                 │
└────────────────────────────────────────────────────────────────────────────┘
┌ kuyruk (4) · tekrar: tümü · karıştır: açık ────────────────────────────────┐
│   Pink Floyd - Speak to Me                                                 │
│   Pink Floyd - Breathe (In the Air)                                        │
│ ▸ Pink Floyd - Time                                                        │
│   Pink Floyd - The Great Gig in the Sky                                    │
└────────────────────────────────────────────────────────────────────────────┘
 boşluk duraklat · n/b sonraki/önceki · ↑↓ seç · enter çal · s karıştır · r tekrar · q çık
```

### 🖥️ Masaüstünde de aynısı

Kuyruk, arama, istatistikler, sleeve, sağlayıcı ve eklenti yönetimi tek
pencerede. Export arşivini pencereye sürükle, bırak. Kısayolları görmek için
<kbd>?</kbd>.

Arayüz **kullanıcının yazabildiği CSS temalarını** destekler: 14 semantik
token, sürümlenmiş bir sözleşme, iki referans tema.
→ [tema yazma rehberi](crates/headshell/themes/README.tr.md)

---

## Kurulum

> ### ⚠️ Beta ne demek
>
> Çekirdek yetenekler çalışıyor ve **466 test** altında duruyor; ama hiçbir
> sürüm henüz senin makinen dışında bir yerde yaşamadı. İçe aktardığın
> export dosyasına dokunulmaz, o yüzden veri kaybı beklenmiyor — ama
> kütüphane şeması sürümler arasında değişebilir.

**Masaüstü paketleri** (`.deb`, `.rpm`, `.AppImage`, `.dmg`, `.msi`) her sürüm
etiketinde üretilir ve [sürüm sayfasına](https://github.com/headshell/headshell/releases)
eklenir. macOS ve Windows paketleri imzasızdır: macOS'ta sağ tık → Aç,
Windows'ta SmartScreen → Yine de çalıştır.

**Hangi sistemde ne kadar sınandı** — "çalışıyor" ile "derleniyor" ayrı
şeyler, tablo ikisini ayırıyor:

| Sistem | Hazır paket | Durum |
|---|---|---|
| Linux x86_64 | `.deb`, `.rpm`, AppImage, CLI arşivi, AUR | **Sınanıyor** — her değişiklikte CI; gerçek kullanımda ses çalma dahil |
| Windows 10/11 x64 | `.msi`, kurulum `.exe`'si, CLI arşivi | **CI'da derleme ve testler** (D-070); gerçek bir masaüstünde elle denenmedi |
| macOS, Apple Silicon ve Intel (evrensel ikili) | `.dmg`, CLI arşivi | **CI'da derleme ve testler** Apple Silicon'da (D-070); Intel yalnızca derleniyor; elle denenmedi |
| Linux aarch64, musl (Alpine) | yok — kaynaktan | **Denenmedi** — derlenmesi beklenir |
| FreeBSD, NetBSD, OpenBSD, DragonFly | yok — kaynaktan | **Denenmedi (canary)** — derleme için `libclang` gerekir; ses ve masaüstü kabuğunun orada çalışıp çalışmadığı bilinmiyor |

Hazır Linux paketlerinin istediği en düşük glibc: **2.35** (Ubuntu 22.04,
Debian 12, Fedora 36 ve sonrası). `v0.0.1-beta` Ubuntu 24.04'te derlenmişti
ve **2.39** istiyor — Debian 12'de açılmıyor; sonraki sürümden itibaren
paketler 22.04'te derleniyor.

Veri dizini (kütüphane, eklentiler, ayarlar) her sistemin kendi yerinde durur;
`HEADSHELL_DATA_DIR` hepsinde önce gelir, `headshell diag` kullanılanı yazar:

| Sistem | Veri dizini |
|---|---|
| Linux, BSD | `~/.local/share/headshell` (`$XDG_DATA_HOME` tanımlıysa onun altı) |
| macOS | `~/Library/Application Support/headshell` |
| Windows | `%LOCALAPPDATA%\headshell` |

**Arch Linux** — AUR'da iki yol var; `-bin` olan derleme beklemez:

```bash
paru -S headshell          # masaüstü, kaynaktan derler
paru -S headshell-cli      # komut satırı
paru -S headshell-bin      # aynısının derlenmiş hâli
paru -S headshell-cli-bin  # CLI'nin derlenmiş hâli
```

**Kaynaktan:**

```bash
git clone https://github.com/headshell/headshell.git
cd headshell
cargo build --release

cp target/release/headshell ~/.local/bin/   # komut satırı
cargo run -p headshell                      # masaüstü penceresi
```

<details>
<summary><b>Derleme gereksinimleri</b></summary>

<br>

Rust 1.87+ (2024 edition) ve bir C derleyicisi (SQLite ile eklenti motoru
kaynaktan derleniyor). Linux'ta masaüstü kabuğu için:
`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libasound2-dev`, `librsvg2-dev`,
`patchelf`. Windows'ta MSVC araç zinciri, macOS'ta Xcode komut satırı
araçları yeter. BSD'lerde ek olarak `libclang` gerekir (eklenti motorunun
bağlaması orada derleme anında üretiliyor).

Hazır Linux paketleri **Ubuntu 22.04'te** derlenir: bir ikili, derlendiği
sistemin glibc'sinden eskisinde açılmaz. Kendin derlersen ikili, derlediğin
makineden eski sistemlerde açılmayabilir.

Eklentiler için **hiçbir şey kurman gerekmez.** Eklenti motoru (QuickJS)
`headshell`'un içinde geliyor. `headshell plugin install <ad>` eklentiyi
[katalogdan](https://github.com/headshell/plugins) indirir ve ihtiyaç duyduğu
araçları (YouTube Music için yt-dlp) motor kurar — senin platformunun kendi
kendine yeten ikilisini, sabitlenmiş sürümden ve sha256 doğrulayarak, senin
veri dizinine. Python, `pip`, root gerekmez.

</details>

---

## Nasıl kullanılır

<details>
<summary><b>Spotify verini nasıl istersin?</b> (tıkla)</summary>

<br>

1. Spotify → **Hesap → Gizlilik ayarları**
2. **"Genişletilmiş akış geçmişi"** kutusunu işaretle, talep et
3. Birkaç gün içinde e-postana bir indirme bağlantısı gelir

Bu bir GDPR hakkı; Spotify vermek zorunda ve bunun için bir API anahtarına
ihtiyacın yok.

</details>

```bash
# 1 — geçmişini içeri al
headshell import spotify_verilerim.zip

# 2 — bak bakalım neymiş
headshell stats --year 2024 --top 10

# 3 — kartını üret
headshell sleeve --year 2024 --out 2024.png

# 4 — müziğini bağla ve çal
export HEADSHELL_MUSIC_DIRS=~/Müzik
headshell provider scan
headshell play "Portishead" --all --tui
```

Uzak sunucu bağlamak:

```bash
headshell provider add subsonic --url https://muzik.evim.com --user ahmet --name ev
headshell provider add jellyfin --url https://jf.evim.com --user ahmet
headshell provider test ev
```

Bir şey ters giderse:

```bash
headshell diag     # son çalıştırmanın ortamı, aşaması, hata zinciri — tek blok
```

<details>
<summary><b>Bütün komutlar</b></summary>

<br>

| Komut | Ne yapar |
| :--- | :--- |
| `headshell import <zip\|dizin>` | Export arşivini içe aktarır |
| `headshell stats [--year N] [--top N]` | Dinleme istatistikleri |
| `headshell sleeve [--year N] [--format square\|story]` | Paylaşılabilir kart üretir |
| `headshell resolve "<sanatçı> - <başlık>"` \| `--file <ses>` | Tek parçayı kimlik zincirinden geçirir |
| `headshell library search <sorgu>` | Kütüphanede tam metin arama |
| `headshell play <sorgu> [--all] [--shuffle] [--tui]` | Çalar |
| `headshell provider list \| test \| scan \| add \| remove \| servers` | Sağlayıcı yönetimi |
| `headshell plugin catalog \| install \| update \| remove` | Eklenti kataloğu: listele, kur, güncelle, kaldır |
| `headshell plugin list \| approve \| disable \| enable \| forget` | Kurulu eklentiler ve onayları |
| `headshell secret list \| set \| remove` | Sır deposu (değerler asla gösterilmez) |
| `headshell diag` | Tanı raporu |

Her komut `--json` destekler:

```bash
headshell stats --year 2024 --json | jq '.report.top_artists[0]'
```

</details>

---

## Nereden çalar

| Kaynak | Durum | Not |
| :--- | :--- | :--- |
| **Yerel dosyalar** | ✅ Çekirdekte | FLAC, MP3, OGG/Vorbis, M4A/AAC |
| **Subsonic / Navidrome** | ✅ Çekirdekte | Gerçek sunucuda doğrulandı |
| **Jellyfin** | ✅ Çekirdekte | Gerçek sunucuda doğrulandı |
| **SoundCloud** | ✅ Eklenti | Hiçbir şey gerekmez |
| **YouTube Music** | ✅ Eklenti | yt-dlp'nin platform ikilisini motor indirir, sha256 doğrular |
| **Torrent (Torznab)** | ⏸️ Park edildi | Eski eklenti protokolüne yazılmıştı; yeni motora sonra taşınacak |
| **Spotify çalma** | ❌ Yok | Ve olmayacak — içe aktarma zaten export dosyasından |

Eklentiler JavaScript'le yazılır ve `headshell`'un içine gömülü **QuickJS**
motorunda koşar: kullanıcının makinesinde Python, Node ya da başka bir
çalışma zamanı gerekmez. Her eklenti neye erişeceğini beyan eder ve onayını
ister — ve beyan **zorlanır**: eklenti yalnızca beyan ettiği adreslere
bağlanabilir, dosya sistemine erişemez. Takılan ya da hata veren bir eklenti
uygulamayı düşürmez.

Eklentiler ayrı bir depoda, [**`headshell/plugins`**](https://github.com/headshell/plugins)
kataloğunda yaşar; uygulama listeyi oradan okur:

```bash
headshell plugin catalog              # ne var, ne kurulu, ne güncellenebilir
headshell plugin install soundcloud   # indirir, her dosyanın sha256'sını doğrular
headshell plugin approve soundcloud   # kurulan eklenti onay bekler
headshell plugin update               # katalogdan kurulanları güncelle
```

Katalog yalnızca sen isteyince okunur. Elle kurduğun ya da elle
değiştirdiğin bir eklentinin üstüne güncelleme yazmaz.
→ [eklenti yazma rehberi](docs/writing-plugins.tr.md)

---

## Gizlilik

* **Telemetri yok.** Kodda böyle bir şey yok; arayabilirsin.
* **Ağ varsayılan kapalı.** `--online` demedikçe kimlik çözümlemesi hiçbir
  servise sormaz. Bir export'u içe aktarmak seni sessizce ağa bağlamaz.
  Eklenti kataloğu da yalnızca `plugin catalog`, `install` ya da `update`
  dediğinde okunur — açılışta, arka planda "güncelleme var mı" diye sorulmaz.
* **Parolan diske yazılmaz.** Komut satırına da yazılmaz (kabuk geçmişine ve
  `ps` çıktısına sızardı). Subsonic'te ondan bir token türetilir, Jellyfin'de
  bir erişim anahtarı alınır; saklanan bunlardır.
* **Ama bir şifreleme vaadi verilmiyor:** türetilmiş token o sunucu için
  parolanın yerine geçer ve `servers.json` içinde `0600` izinli düz metin
  olarak durur. Koruma dosya izninin verdiği kadar; diskine erişen birine
  karşı değil.

---

## Yol haritası

- [x] **Kimlik & istatistik** — içe aktarma, kanonik kimlik zinciri, SQLite, istatistik motoru
- [x] **Sleeve** — SVG/PNG kart, kare ve hikâye
- [x] **Çalma** — yerel dosyalar, Subsonic, Jellyfin, scrobbler, TUI
- [x] **Eklentiler** — gömülü QuickJS motoru, zorlanan izinler, iki sağlayıcı, AcoustID parmak izi
- [x] **Masaüstü** — Tauri arayüzü ve sürümlenmiş CSS tema sözleşmesi
- [ ] **Odalar** — birlikte senkron dinleme; ses röle edilmez, yalnızca zaman çapası geçer
- [ ] **Sosyal graf** — arkadaşlık kurulmaz, birlikte dinlemelerden türetilir
- [ ] **Mobil** — `uniffi` ile iOS ve Android

---

## Sıkça sorulanlar

<details>
<summary><b>Spotify şifremi ya da API anahtarımı vermem gerekiyor mu?</b></summary>
<br>
Hayır. <code>headshell</code> Spotify API'sine hiç bağlanmaz. GDPR kapsamında
talep ettiğin export zip'ini yerelden okur — bu, sağlayıcının geliştirici
şartlarıyla kısıtlayamayacağı bir hak.
</details>

<details>
<summary><b>Bir müzik dosyasını silersem dinleme geçmişim gider mi?</b></summary>
<br>
Hayır. "Ne dinledin" ile "şu an ne çalabilirsin" ayrı tablolarda durur.
Dosyayı silsen de, sunucuyu kapatsan da, bir eklentiyi kaldırsan da geçmişin
ve istatistiklerin yerinde kalır.
</details>

<details>
<summary><b>İnternetsiz çalışır mı?</b></summary>
<br>
Evet. İçe aktarma, yerel çalma, istatistik ve sleeve üretimi tamamen
çevrimdışı çalışır. Ağ yalnızca uzak sunucular, eklentiler, eklenti kataloğu
ve <code>--online</code> ile açılan kimlik halkaları için gerekir.
</details>

<details>
<summary><b>Neden bir "müzik uygulaması" daha?</b></summary>
<br>
Çünkü bu bir müzik uygulaması değil, bir <b>dinleme kimliği</b> katmanı.
Altındaki ses kaynağı değişebilir — hatta değişmesi bekleniyor. Değişmeyen
şey, kimin ne dinlediğinin kayıtlı olduğu yer.
</details>

---

## Katkı

Hata bildirimleri, öneriler ve yamalar açık. Geliştirici kuralları ve test
adımları: [`CONTRIBUTING.tr.md`](CONTRIBUTING.tr.md)

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Üçü de temiz geçmeden hiçbir değişiklik "bitti" sayılmaz.

## Lisans

**MIT** ([LICENSE-MIT](LICENSE-MIT)) **veya** **Apache-2.0**
([LICENSE-APACHE](LICENSE-APACHE)) — dilediğini seç.
