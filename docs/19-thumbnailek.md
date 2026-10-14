# 19 — Klipek előnézete (thumbnail) a UI-ban

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro*

## Az útvonal

CPU-oldali, egyszer fut klippenként — nem kell GPU:

```
VideoReader (AVAssetReader, első frame)
  -> CVPixelBuffer (BGRA sorok kiolvasása)
    -> box-szűrős lekicsinyítés (240x135), közben BGRA -> RGB
      -> JPEG (jpeg-encoder, minőség 75)
```

A box-szűrő minden kimeneti pixelhez a forrás megfelelő blokkját átlagolja,
ezért a lekicsinyítés nem "szemcsés" (nincs aliasing).

## A web-útvonal

```
GET /thumb?deck=0&clip=1     -> image/jpeg (cache-elve)
```

A klipek útvonalait az app adja át a szervernek induláskor
(set_clips), a thumbnail pedig **igény szerint** készül el és a memóriában
marad (Cache-Control: max-age=3600 is megy a böngészőnek). A generálás a
kérés saját szálán fut, tehát a render loopot nem blokkolja.

## Az UI

A clip-kockák mostantól 16:9-es képet mutatnak, a fájlnév a kép alján
olvasható; a futó klip narancs, a várakozó kék keretet kap. A képek
loading="lazy" módon töltődnek.

## Mért eredmény

```
3 klip átadva a UI-nak (thumbnailek)
thumbnail: deck 0 klip 0 (4423 bájt,  39 ms)
thumbnail: deck 0 klip 1 (5971 bájt, 200 ms)   <- 4K forrás
thumbnail: deck 1 klip 0 (3540 bájt,  19 ms)

HTTP 200 · 4423 / 5971 / 3540 bájt
```

A 240x135-ös, minőség 75-ös JPEG 3,5–6 KB. A generálás 19–200 ms között van,
a forrás felbontásától függően (a 4K klip a leglassabb) — de ez egyszer fut.

## Igazolás

A deck 0 / klip 1 (tahoe_4k.mov) előnézete pontosan a videó első kockája:
türkiz víz, sziklák, havas hegyek a háttérben.

## Reprodukálás

```bash
source scripts/env.sh
./target/release/dada-vj --deck ".testmedia/base:16" --serve
curl -s -o /tmp/t.jpg 'http://127.0.0.1:7777/thumb?deck=0&clip=1' && open /tmp/t.jpg

# önálló ellenőrzés egy fájlra:
./target/release/dada-vj --thumb-check .testmedia/hero.mp4
```
