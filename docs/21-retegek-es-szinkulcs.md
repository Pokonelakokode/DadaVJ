# 21 — Rétegek, effekt célréteg és színkulcs (chroma key)

*Dátum: 2026-10-21*

Kétféle kulcsozás él a programban: a **deckenkénti chroma key** (fő út,
ArKaOS-stílus; klip szinten felülírható) és a globális **ISF chroma_key
shader** (alternatíva). Előbbi az adott deck videó-rétegének alfáját
módosítja, utóbbi a shader célrétegén fut — a részletek lentebb.

## Réteg-sorrend (Z-order)

A kép **alulról felfelé** épül: a nagyobb Z-jű réteg később rajzolódik, tehát
felül van. A deck-rétegek sorrendje a **Rétegek (fentről lefelé)** panelen
állítható:

- a sorok **drag & drop**-pal átrendezhetők (fent = legfelső réteg),
- a ▲ / ▼ gombok ugyanezt teszik eggyel feljebb / lejjebb,
- az audio-vizuál réteg (ha be van kapcsolva) statikusan a legfelső sor.

A UI a sorrendet alulról felfelé, deck-indexek listájaként küldi
(`set_layer_order`, pl. `[0, 1, 2]` = a 0. deck alul, a 2. felül). Az
állapotban deck-enként `z` (0 = alsó), a teljes lista `layer_order` néven
szerepel. A sorrend csak a **rajzolás sorrendjét** változtatja, a deckek
klipjeit nem.

## Az ISF effekt célrétege

A Master panel **Shader cél réteg** választója dönti el, hova kerül az ISF
effekt:

| Beállítás | Mit jelent |
|---|---|
| **Legfelső - teljes kompozit** (alapértelmezés, -1 / null) | Az effekt a már összeállt kép **fölött** jelenik meg, a saját blend módjával és opacity-jével. Képi bemenete a lentebbi rétegek képe. |
| **N. deck** (0, 1, 2, ...) | Az effekt az **N. deck helyén** rajzolódik: a deck klipje helyett a shader kimenete látszik. Képi bemenete az adott deck képe, a deck opacity-je és a réteg-sorrend itt is érvényesül. |

A célréteg-csere nem módosítja a decket: csak a megjelenítés útja tér el.
Ha a shader célréteg egy deck, a deck rétege a saját Z-helyén marad — az
alatta lévő rétegek változatlanul látszanak ott, ahol a shader átlátszó.

## Shader Alpha módok

A **Shader Alpha** választó azt mondja meg, mit jelent az ISF kimenet alpha
csatornája:

| Opció | Alpha | Tipikus használat |
|---|---|---|
| **Fedés (opacity)** (`coverage`) | a réteg opacity-je; a shader által írt alpha nem számít | teljes képernyős effekt, átúsztatás, opacity-vezérlés |
| **Shader alpha (átlátszás / kulcsolás)** (`shader`) | a shader által írt alpha (kulcsozás); az opacity továbbra is szorzó | chroma key, maszkolás, kivágás |

Kulcsozáshoz tehát **Shader Alpha = shader** kell. Fedés módban az effekt
mindenhol "teli", és a blend dönti el, hogyan kerül a képbe.

> **Megjegyzés:** Normal blend + 100% fedés (opacity 1.0) mellett a felső
> réteg **eltakarja** az alatta lévő képet. Ha a shader célrétege egy deck,
> akkor az alatta lévő deck csak akkor látszik, ha a shader alpha < 1
> (Shader Alpha = shader), vagy ha a deck opacity-jét 1 alá vesszük.

## Deckenkénti chroma key (ArKaOS-stílus)

A deckenkénti chroma key a **fő színkivételi út**: minden deck saját
kulcsszínt kap, és a kulcsszínhez közeli pixelek az adott deck
**videó-rétegén** átlátszóvá válnak — így az alatta lévő deck (vagy a
legalsó kompozit) látszik. Nem kell hozzá shadert betölteni, a beállítás
deckenként él, és a projektfájlban mentődik.

### Hol állítható

A web UI-ban a **Deck #N** kártya **„Deck — minden klipre”** szakaszában, a
Deck BPM sync sor alatt:

| Vezérlő | Mit állít |
|---|---|
| **Chroma key** (jelölőnégyzet) | az adott deck kulcsozásának be/ki kapcsolása |
| **színválasztó** | a kivett kulcsszín (alapból zöld) |
| **Tol** | tolerance: mennyire térhet el egy pixel a kulcsszíntől, hogy még kivettnek számítson |
| **Soft** | softness: a kulcs szélének lágysága |
| **Spill** | despill: a megtartott, kulcsszínhez közeli pixelek telítettség-csökkentése |

A beállítás a teljes deckre érvényes **alapként** (minden klipre és a
decken belüli crossfade-re); klip szinten felülírható — lásd a
„Klipp-szintű felülírás (öröklés)” részt. Nem érinti a többi decket.
Több decknek külön-külön is lehet kulcsa, akár eltérő kulcsszínnel.

### Mit jelent pontosan

- A kulcsszínhez közeli pixelek **alfája 0** lesz az adott deck videó-
  rétegén; a többi pixel megmarad (a kulcs szélén a spill a színt is
  korrigálhatja), a határon lágy átmenettel.
- A kivett helyen a deck Z-helyén az **alatta lévő réteg** látszik — a
  hátteret tehát egy alacsonyabb deckre kell tenni.
- A kulcs a videó-réteg alfáját állítja, ezért a deck **opacity-je**,
  **crossfade-je** és a **Normal / Add** blend a megszokott módon érvényesül;
  a kulcsolt pixelek ezeknél a blend módoknál átlátszók maradnak. A
  **Multiply / Screen** fix-funkciós blend nem használja az alfát (a
  kulcsolt pixel ott sötétít/színez) — ez a blend-motor ismert korlátja.
- Ha a kulcsolt deck a legalsó, alatta nincs semmi, így a kivett helyen a
  kompozit háttere (üres/fekete) marad.

### Paraméterek

| Paraméter | Alapérték | Tartomány | Mit állít |
|---|---|---|---|
| Chroma key | kikapcsolva | be/ki | az adott deck kulcsozása |
| Kulcsszín | zöld `[0, 1, 0]` (#00FF00) | színválasztó | a kivett szín sRGB-ben (0..1) |
| Tol (tolerance) | 0.28 | 0..1 | ezen belül számít kulcsszínnek; nagyobb = agresszívabb kivétel |
| Soft (softness) | 0.12 | 0..0.5 | a kulcs széle: 0 = kemény, 0.5 = nagyon lágy (smoothstep) |
| Spill | 0.35 | 0..1 | a kulcsszínhez közeli, megtartott pixelek telítettségét a luma felé húzza (zöld visszaverődés eltüntetése) |

### Hogyan működik

A maszk a sRGB színtérben számolt **Cb/Cr krominancia-távolságból**
készül: a tolerance adja a kulcs magját, a softness a smoothstep szélét. A
kivett pixelek alpha-ja 0 lesz; a kulcsszínhez közeli, de megtartott
pixeleknél a spill a telítettséget a luma felé húzza, hogy a széleken ne
maradjon zöld derengés. Mindez a deck videó-rétegének shaderjében fut, a
globális ISF effektlánctól függetlenül; az alpha a kulcsmaszk és a deck
opacity szorzata, ezért a deck opacity/blend/crossfade ezután is
érvényesül.

> **Megjegyzés:** ha a kulcsolt deck egyben az ISF effekt **célrétege**
> (képi bemenettel), az effekt a kulcsolt deck képét kapja meg, és a kivett
> helyen ilyenkor a snapshot háttere látszik, nem az alatta lévő deck.
> A két kulcsozási út egymástól független; deckenkénti kulcshoz nem kell
> ISF effektet betölteni, ez a legegyszerűbb út.

### Gyakorlati beállítás — zöld háttér kivétele

1. Tedd a zöld hátteres klipet egy deckre, a hátteret pedig alá egy másik
   deckre. A Rétegek panelen a kulcsolt deck legyen felül (nagyobb Z).
2. Nyisd ki a felső deck kártyáját, és a „Deck — minden klipre” szakaszban
   kapcsold be a **Chroma key**-t.
3. A kulcsszín álljon a háttér zöldjére: tiszta zöldnél maradhat a
   `#00FF00` alapérték; ha a háttér nem tiszta zöld, a színválasztóval a
   háttér árnyalatára állítsd.
4. **Tol** = 0.3–0.5. Ha maradnak zöld foltok, növeld; ha a téma széle is
   eltűnik, csökkentsd.
5. **Soft** = 0.1 a legtöbb esetben. Kemény, „kivágott” szélhez 0.0–0.05,
   nagyon lágy átmenethez 0.2 körül.
6. Ha a téma szélén zöldes derengés marad, emeld a **Spill**-t
   (0.3–0.5); ha a színek túl fakók lesznek, csökkentsd.
7. Ellenőrizd: a kivett helyen az alatta lévő deck/kompozit látszik. A
   deck opacity és Blend továbbra is állítható; Normal blend + 100%
   opacity mellett a kulcs tisztán átlátszik.

### Projektfájl

A beállítás a projektfájlban a `[[decks]]` bejegyzés alatt, opcionális
mezőkkel él. Ha egy mező hiányzik, az alapértéke érvényes — a korábbi
projektfájlok változatlanul betölthetők. A programból mentett projekt mind
az öt chroma mezőt kiírja; kézzel szerkesztve bármelyik elhagyható.

```toml
[[decks]]
folder = "media/hatter"
# ...

[[decks]]
folder = "media/zold-hatter"
chroma_key = true
chroma_color = [0.0, 1.0, 0.0]
chroma_tolerance = 0.4
chroma_softness = 0.1
chroma_spill = 0.35
```

| Mező | Típus | Alapérték | Mit tartalmaz |
|---|---|---|---|
| `chroma_key` | bool | `false` | be van-e kapcsolva a decken |
| `chroma_color` | `[r, g, b]`, 0..1 sRGB | `[0.0, 1.0, 0.0]` | a kivett kulcsszín |
| `chroma_tolerance` | 0..1 | `0.28` | tolerance (Tol) |
| `chroma_softness` | 0..0.5 | `0.12` | softness (Soft) |
| `chroma_spill` | 0..1 | `0.35` | despill (Spill) |

A `--save` a deckenkénti kulcsbeállításokat is kiírja; a `--project`
betölti őket. A web UI a deck `deck` parancsával (`chroma_key`,
`chroma_color`, `chroma_tolerance`, `chroma_softness`, `chroma_spill`)
állítja; az állapot JSON-ban ugyanezek a mezők a `decks[i]` alatt
szerepelnek.

### Klipp-szintű felülírás (öröklés)

A deck beállítása az **alap** (default) minden klipre, de egyes klipeken
felülírható: a klip örökölheti a deck kulcsát, saját kulcsot kaphat, vagy
kikapcsolhatja magán a kulcsot. Az override csak a klip lejátszásakor él —
a deck beállítása a többi klipre és az új klipekre változatlan
alapértelmezés marad.

#### A három mód

| Mód (UI) | `chroma_mode` | Mit jelent |
|---|---|---|
| **Deck beállítás használata (öröklés)** — alap | `inherit` | a klip a deck chroma key beállítását használja (szín, Tol, Soft, Spill); ha a decken a kulcs ki van kapcsolva, a klipen sincs kulcs |
| **Egyedi kulcs a klipre** | `custom` | a klip saját kulcsszíne és tolerance/softness/spill értékei élnek; a deck beállítása erre a klipre nem hat |
| **Nincs kulcs ezen a klipen** | `off` | a klipen nincs kulcs akkor sem, ha a decken a chroma key be van kapcsolva |

Az **effektív beállítás** szabálya:

- `custom` → a klip saját értékei (a deck beállításától függetlenül),
- `off` → nincs kulcs,
- `inherit` → a deck beállítása.

#### Crossfade

Klipek közötti crossfade alatt a kifutó klip a saját, a bejövő klip a
saját effektív beállítását használja, így a két klip kulcsa nem ütközik:
az egyik lehet `off` vagy `custom`, a másik `inherit`, a váltás alatt
mindkettő a maga szabálya szerint kulcsol.

#### Hol állítható

A **Klip Inspektorban**: a klip csempéjén a **⚙** gomb nyitja a kijelölt
klip beállításait, ott a **Klip chroma key** szekcióban:

- **mód** select: „Deck beállítás használata (öröklés)” / „Egyedi kulcs a
  klipre” / „Nincs kulcs ezen a klipen”,
- **színválasztó** — a klip kulcsszíne,
- **Tol / Soft / Spill** csúszkák (csak `custom` módban aktívak),
- **deck alap** hint: „Deck alap: BE/KI · #RRGGBB · Tol … · Soft … · Spill …”
  (az első jelző a deck kulcs be/ki állapota, mert öröklésnél ez dönt).

A klip csempéjén badge mutatja az override-ot: **CK** = egyedi kulcs
(`custom`), **CK✕** = nincs kulcs (`off`); öröklésnél (`inherit`) nincs
badge.

Az **Alkalmaz** gomb a `set_clip` parancsot küldi (`deck`, `clip`,
`chroma_mode`, `chroma_color`, `chroma_tolerance`, `chroma_softness`,
`chroma_spill`); az állapot JSON-ban a `decks[].clips[]` alatt ugyanezek
a mezők szerepelnek.

#### Projektfájl — klip mezők

A klip mezői a `[[decks.clips]]` bejegyzések alatt élnek, opcionálisak
(default: `inherit`). Hiányzó érték esetén a fenti alapérték érvényes
(zöld kulcsszín, 0.28 / 0.12 / 0.35); a hiányzó `chroma_mode` =
`inherit`.

```toml
[[decks]]
folder = "media/zold-hatter"
chroma_key = true
chroma_color = [0.0, 1.0, 0.0]
chroma_tolerance = 0.4
chroma_softness = 0.1
chroma_spill = 0.35

# egyedi kulcs a klipre (a deck beállításától függetlenül)
[[decks.clips]]
name = "zold-hatter.mp4"
chroma_mode = "custom"
chroma_color = [0.1, 0.9, 0.2]
chroma_tolerance = 0.45
chroma_softness = 0.08
chroma_spill = 0.4

# nincs kulcs ezen a klipen, a deck kulcsa ellenére sem
[[decks.clips]]
name = "masik-jelenet.mp4"
chroma_mode = "off"

# öröklés: nincs chroma_mode -> a deck beállítása él
[[decks.clips]]
name = "harmadik.mp4"
```

| Mező | Típus | Alapérték | Mit tartalmaz |
|---|---|---|---|
| `chroma_mode` | `"inherit"` / `"custom"` / `"off"` | `"inherit"` | a klip kulcsának módja |
| `chroma_color` | `[r, g, b]`, 0..1 sRGB | `[0.0, 1.0, 0.0]` | a klip kulcsszíne (`custom`) |
| `chroma_tolerance` | 0..1 | `0.28` | tolerance (`custom`) |
| `chroma_softness` | 0..0.5 | `0.12` | softness (`custom`) |
| `chroma_spill` | 0..1 | `0.35` | despill (`custom`) |

A `--save` a klip-szintű mezőket is kiírja, a `--project` visszatölti.

#### Nincs ütközés a deck beállításával

A deck-szintű beállítás marad az alap; a klip override csak a saját
lejátszásakor érvényes, és a deck beállítását nem módosítja. Így ugyanazon
a decken lehet zöld hátteres, egyedi kulcsú és kulcs nélküli klip is, a
deck alapbeállítása pedig a többi és a jövőbeli klipekre is érvényben
marad. Az öröklés szabályát egységtesztek fedik (inherit / custom / off /
hiányzó mező, projekt roundtrip).

### Ellenőrzés

```bash
source scripts/env.sh
cargo run -p dada-vj -- --chroma-check
```

A headless GPU teszt négy esetet ellenőriz: zöld kulcsszínnel a kivett
szín átlátszó lesz, kikapcsolt kulcs mellett a kép látszik, más
kulcsszínnel pedig nem történik kulcsozás; 50% opacity mellett a réteg
félig áttetsző.

### Függetlenség a globális ISF chroma_key shadertől

A deckenkénti chroma key és a globális `chroma_key.isf` effekt két külön
dolog — más helyen futnak, és nem írják felül egymást:

| | Deckenkénti chroma key (fő út) | ISF `chroma_key.isf` (alternatíva) |
|---|---|---|
| Hol fut | az adott deck **videó-rétegén** | az ISF effekt **célrétegén** (legfelső kompozit vagy N. deck) |
| Mit módosít | a deck videó-rétegének alfáját | a shader kimenetének alfáját; `Shader Alpha = shader` kell hozzá |
| Beállítás | Deck kártya (deckenként), a projektben mentve | shader paraméterek + Shader cél réteg / Shader Alpha |
| Használat | shader nélkül is megy, ez a deckenkénti alapmód | ha a kulcsozás egy effektlánc része, a célrétegen |

A per-deck key a deck opacity/blend/crossfade mellett is működik, mert a
videó-réteg alfáját állítja, nem a kész kompozitét.

## A chroma_key.isf effekt (alternatíva)

`shaders/chroma_key.isf` ugyanezt a kulcsozást végzi el ISF effektként:
a kulcsszínhez (alapból zöld) közeli pixeleket átlátszóvá teszi, így az
alatta lévő réteg kilátszik. A maszk a **Cb/Cr krominancia-síkban** mért
távolságból készül (a luma különbség csak kis súllyal számít), ezért a
megvilágítás ingadozása nem töri el a kulcsozást; a szélen smoothstep ad
lágy átmenetet. A kimenet `vec4(rgb, mask)` — az alpha hordozza a
kulcsot, ezért kell a **Shader Alpha = shader** mód.

| Paraméter | Alapérték | Mit állít |
|---|---|---|
| `key_color` | zöld [0, 1, 0, 1] | a kivett kulcsszín (kék háttérnél állítsd kékre) |
| `tolerance` | 0.28 | ezen belül számít kulcsszínnek (nagyobb = agresszívabb kivétel) |
| `softness` | 0.12 | a kulcs széle (0 = kemény, 0.5 = nagyon lágy) |
| `spill` | 0.35 | a kulcsszínhez közeli pixelek telítettségét a luma felé húzza (zöld visszaverődés eltüntetése) |
| `invert` | false | fordított kulcs: a háttér marad, a téma tűnik el |

Használat: a zöld hátteres klipet egy deckre, a hátteret alá teszed, majd
a Master panelen a **Shader cél réteg** az a deck lesz, ahol a kulcs fut,
és **Shader Alpha = shader**. Normal blend + 100% opacity mellett a shader
alpha dönt; a célréteg és az alpha mód részletei feljebb olvashatók.

Reprodukálás:

```bash
source scripts/env.sh
cargo run -p dada-vj -- --shader shaders/chroma_key.isf --shader-check
```

A `--shader-check` szintetikus bemenettel, pixel-szinten igazolja, hogy a
kulcsszín tényleg átlátszó lesz (és `invert` módban megmarad). Élőben:

```bash
cargo run --release -p dada-vj -- \
  --deck "media/hatter:16" --deck "media/zold-hatter:16" \
  --shader shaders/chroma_key.isf --serve
# böngésző: http://127.0.0.1:7777/  (Master panel: Shader cél réteg, Shader Alpha)
```
