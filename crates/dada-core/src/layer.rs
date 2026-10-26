//! Deck-rétegek Z-sorrendje.
//!
//! A [`LayerOrder`] a deck-indexek sorrendjét tartja **alulról felfelé**:
//! a 0. elem a legalsó réteg, amely először rajzolódik, a nagyobb indexű
//! elemek később kerülnek a kép tetejére. A kompozit tehát a vektor elejétől
//! a vége felé haladva épül fel, a nagyobb Z nagyobb prioritást (későbbi
//! rajzolást) jelent.
//!
//! A modul szándékosan GPU- és időmentes: csak a deck-indexek permutációját
//! kezeli, így determinisztikusan tesztelhető.

/// A deck-rétegek Z-sorrendje, alulról felfelé.
///
/// * `order[0]` = a legalsó réteg deck-indexe (először rajzolva),
/// * `order[len - 1]` = a legfelső réteg (utoljára rajzolva, minden mást
///   felülír).
///
/// Érvényes állapotban a vektor pontosan a `0..len` indexek permutációja,
/// de a `LayerOrder` a köztes (pl. deck törlése utáni) állapotokat is kezeli:
/// az `ensure` a hiányzó indexeket pótolja, a `set` pedig csak teljes
/// permutációt fogad el.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerOrder {
    order: Vec<usize>,
}

impl LayerOrder {
    /// Identitás-sorrend `count` deckhez: `[0, 1, ..., count - 1]`.
    ///
    /// Ez a „nincs átrendezés” állapot: a deck indexe egyben a Z-szintje.
    pub fn new(count: usize) -> Self {
        Self {
            order: (0..count).collect(),
        }
    }

    /// Sorrend egy szeletből, ha az pontosan a `0..count` permutációja.
    ///
    /// Érvénytelen bemenet (tartományon kívüli, duplikált, hiányzó elem,
    /// vagy eltérő hossz) esetén az identitás-sorrendet adja vissza.
    pub fn from_slice(order: &[usize], count: usize) -> Self {
        let mut this = Self::new(count);
        this.set(order, count);
        this
    }

    /// A sorrend alulról felfelé (a 0. elem a legalsó réteg).
    pub fn as_slice(&self) -> &[usize] {
        &self.order
    }

    /// A rétegek száma.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Igaz, ha nincs egyetlen réteg sem.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Benne van-e a `deck` index a sorrendben.
    pub fn contains(&self, deck: usize) -> bool {
        self.order.contains(&deck)
    }

    /// A `deck` pozíciója a sorrendben (0 = legalsó), ha benne van.
    pub fn position(&self, deck: usize) -> Option<usize> {
        self.order.iter().position(|&d| d == deck)
    }

    /// A sorrend összhangba hozása `count` deckkel.
    ///
    /// Először eldobja a tartományon kívüli (`>= count`) és a duplikált
    /// elemeket (az első előfordulás marad meg), majd a hiányzó
    /// `0..count` indexeket növekvő sorrendben a végére fűzi. Így az
    /// eredmény mindig a `0..count` permutációja.
    pub fn ensure(&mut self, count: usize) {
        let mut seen = vec![false; count];
        let mut kept = Vec::with_capacity(count);
        for &deck in &self.order {
            if deck < count && !seen[deck] {
                seen[deck] = true;
                kept.push(deck);
            }
        }
        for (deck, present) in seen.iter().enumerate() {
            if !present {
                kept.push(deck);
            }
        }
        self.order = kept;
    }

    /// A sorrend beállítása, de csak ha `order` pontosan a `0..count`
    /// permutációja.
    ///
    /// Siker esetén eltárolja és `true`-t ad; érvénytelen bemenetnél
    /// (rossz hossz, duplikált vagy tartományon kívüli elem) a sorrend
    /// változatlan marad és `false`-t ad.
    pub fn set(&mut self, order: &[usize], count: usize) -> bool {
        if order.len() != count {
            return false;
        }
        let mut seen = vec![false; count];
        for &deck in order {
            if deck >= count || seen[deck] {
                return false;
            }
            seen[deck] = true;
        }
        self.order.clear();
        self.order.extend_from_slice(order);
        true
    }

    /// A következő szabad index (`len`) hozzáfűzése a legfelső rétegként.
    ///
    /// Új deck létrehozásakor használatos: az új deck a legtetejére kerül.
    pub fn push(&mut self) {
        self.order.push(self.order.len());
    }

    /// A `deck` eltávolítása a sorrendből.
    ///
    /// A nála nagyobb deck-indexek 1-gyel csökkennek, mert a deckek
    /// újraszámozódnak a törlés után. Ha a `deck` nem szerepel a
    /// sorrendben, a művelet nem csinál semmit.
    pub fn remove(&mut self, deck: usize) {
        let Some(pos) = self.position(deck) else {
            return;
        };
        self.order.remove(pos);
        for d in &mut self.order {
            if *d > deck {
                *d -= 1;
            }
        }
    }

    /// A `deck` áthelyezése a `target` pozícióra (0 = legalsó réteg).
    ///
    /// A `target` a `0..len - 1` tartományba kerül vágásra. `false`-t ad,
    /// ha a `deck` nincs a sorrendben, vagy ha a pozíció nem változna.
    pub fn move_to(&mut self, deck: usize, target: usize) -> bool {
        let Some(pos) = self.position(deck) else {
            return false;
        };
        let target = target.min(self.order.len() - 1);
        if pos == target {
            return false;
        }
        self.order.remove(pos);
        self.order.insert(target, deck);
        true
    }

    /// Egy réteggel feljebb mozgatás: a szomszédossal cserél.
    ///
    /// „Felfelé” = nagyobb Z = később rajzolva, tehát feljebb kerül a
    /// kompozitban. A legfelső réteg már nem mozdul (`false`).
    pub fn move_up(&mut self, deck: usize) -> bool {
        let Some(pos) = self.position(deck) else {
            return false;
        };
        if pos + 1 >= self.order.len() {
            return false;
        }
        self.order.swap(pos, pos + 1);
        true
    }

    /// Egy réteggel lejjebb mozgatás: a szomszédossal cserél.
    ///
    /// „Lefelé” = kisebb Z = korábban rajzolva, tehát lejjebb kerül a
    /// kompozitban. A legalsó réteg már nem mozdul (`false`).
    pub fn move_down(&mut self, deck: usize) -> bool {
        let Some(pos) = self.position(deck) else {
            return false;
        };
        if pos == 0 {
            return false;
        }
        self.order.swap(pos, pos - 1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_ordering() {
        let l = LayerOrder::new(4);
        assert_eq!(l.as_slice(), &[0, 1, 2, 3]);
        assert_eq!(l.len(), 4);
        assert!(!l.is_empty());
        assert!(l.contains(2));
        assert!(!l.contains(4));
        assert_eq!(l.position(3), Some(3));
        assert_eq!(l.position(9), None);

        // Alapértelmezés = 0 réteg.
        assert_eq!(LayerOrder::default(), LayerOrder::new(0));
        assert!(LayerOrder::default().is_empty());
        assert_eq!(LayerOrder::default().as_slice(), &[] as &[usize]);
    }

    #[test]
    fn push_appends_next_free_index() {
        let mut l = LayerOrder::default();
        l.push();
        l.push();
        assert_eq!(l.as_slice(), &[0, 1]);

        // Permutált sorrendnél is a len a következő szabad index.
        let mut l = LayerOrder::from_slice(&[1, 0], 2);
        l.push();
        assert_eq!(l.as_slice(), &[1, 0, 2]);
    }

    #[test]
    fn move_to_reorders() {
        let mut l = LayerOrder::new(4);
        // A 0. decket a 2. pozícióra (alulról számolva).
        assert!(l.move_to(0, 2));
        assert_eq!(l.as_slice(), &[1, 2, 0, 3]);
        // Vissza a legaljára.
        assert!(l.move_to(0, 0));
        assert_eq!(l.as_slice(), &[0, 1, 2, 3]);
        // Ugyanaz a pozíció: nincs változás.
        assert!(!l.move_to(1, 1));
        assert_eq!(l.as_slice(), &[0, 1, 2, 3]);
        // Nem létező deck: nincs változás.
        assert!(!l.move_to(9, 1));
        assert_eq!(l.as_slice(), &[0, 1, 2, 3]);
        // A tartományon túli cél a legfelső pozícióra vágódik.
        assert!(l.move_to(0, 99));
        assert_eq!(l.as_slice(), &[1, 2, 3, 0]);
        // Már ott van: nincs változás.
        assert!(!l.move_to(0, 99));
        assert_eq!(l.as_slice(), &[1, 2, 3, 0]);
    }

    #[test]
    fn move_up_and_down_swap_with_neighbour() {
        let mut l = LayerOrder::new(3);
        // Felfelé = nagyobb Z.
        assert!(l.move_up(0));
        assert_eq!(l.as_slice(), &[1, 0, 2]);
        assert!(l.move_up(0));
        assert_eq!(l.as_slice(), &[1, 2, 0]);
        // A legfelső már nem mehet feljebb.
        assert!(!l.move_up(0));
        assert_eq!(l.as_slice(), &[1, 2, 0]);

        // Lefelé = kisebb Z.
        assert!(l.move_down(0));
        assert_eq!(l.as_slice(), &[1, 0, 2]);
        assert!(l.move_down(0));
        assert_eq!(l.as_slice(), &[0, 1, 2]);
        // A legalsó már nem mehet lejjebb.
        assert!(!l.move_down(0));
        assert_eq!(l.as_slice(), &[0, 1, 2]);

        // Nem létező deck egyik irányban sem mozgatható.
        assert!(!l.move_up(9));
        assert!(!l.move_down(9));
        assert_eq!(l.as_slice(), &[0, 1, 2]);
    }

    #[test]
    fn remove_shifts_larger_indices_down() {
        // Identitásból törlés: a nagyobb indexek eggyel csökkennek.
        let mut l = LayerOrder::new(4);
        l.remove(1);
        assert_eq!(l.as_slice(), &[0, 1, 2]);

        // Permutált sorrendből törlés: a pozíció törlődik, a nagyobb
        // értékek csökkennek; a relatív sorrend megmarad.
        let mut l = LayerOrder::from_slice(&[2, 0, 3, 1], 4);
        l.remove(1);
        assert_eq!(l.as_slice(), &[1, 0, 2]);

        // Nem létező deck: nincs változás.
        l.remove(3);
        assert_eq!(l.as_slice(), &[1, 0, 2]);

        // Az utolsó (legfelső) törlése.
        l.remove(2);
        assert_eq!(l.as_slice(), &[1, 0]);
    }

    #[test]
    fn ensure_fixes_invalid_entries() {
        // Üres sorrend feltöltése.
        let mut l = LayerOrder::default();
        l.ensure(3);
        assert_eq!(l.as_slice(), &[0, 1, 2]);

        // Hiányzó indexek hozzáfűzése a végére, növekvő sorrendben.
        let mut l = LayerOrder { order: vec![2, 0] };
        l.ensure(4);
        assert_eq!(l.as_slice(), &[2, 0, 1, 3]);

        // Duplikált és tartományon kívüli elemek eldobása, majd a
        // hiányzók pótlása.
        let mut l = LayerOrder {
            order: vec![2, 0, 2, 7, 0, 5],
        };
        l.ensure(3);
        assert_eq!(l.as_slice(), &[2, 0, 1]);

        // Érvényes sorrendet nem rendez át.
        let mut l = LayerOrder::from_slice(&[3, 1, 0, 2], 4);
        l.ensure(4);
        assert_eq!(l.as_slice(), &[3, 1, 0, 2]);

        // Kevesebb deckre szűkítés: a kilógók eldobása.
        let mut l = LayerOrder::new(5);
        l.ensure(2);
        assert_eq!(l.as_slice(), &[0, 1]);

        // Nulla deck: üres sorrend.
        let mut l = LayerOrder::new(3);
        l.ensure(0);
        assert!(l.is_empty());
    }

    #[test]
    fn set_accepts_only_permutations() {
        let mut l = LayerOrder::new(3);
        assert!(l.set(&[2, 0, 1], 3));
        assert_eq!(l.as_slice(), &[2, 0, 1]);

        // Rossz hossz.
        assert!(!l.set(&[0, 1], 3));
        // Duplikált elem.
        assert!(!l.set(&[0, 0, 2], 3));
        // Tartományon kívüli elem.
        assert!(!l.set(&[0, 1, 3], 3));
        // Túl hosszú bemenet.
        assert!(!l.set(&[0, 1, 2, 3], 3));

        // Érvénytelen bemenet után a sorrend változatlan.
        assert_eq!(l.as_slice(), &[2, 0, 1]);

        // Az üres permutáció érvényes count = 0 esetén.
        assert!(l.set(&[], 0));
        assert!(l.is_empty());
    }

    #[test]
    fn from_slice_falls_back_to_identity() {
        // Érvényes permutáció: átveszi.
        assert_eq!(LayerOrder::from_slice(&[2, 0, 1], 3).as_slice(), &[2, 0, 1]);
        // Duplikált.
        assert_eq!(LayerOrder::from_slice(&[2, 0, 2], 3), LayerOrder::new(3));
        // Tartományon kívüli.
        assert_eq!(LayerOrder::from_slice(&[0, 1, 5], 3), LayerOrder::new(3));
        // Hiányzó / rövid.
        assert_eq!(LayerOrder::from_slice(&[0, 1], 3), LayerOrder::new(3));
        // Túl hosszú.
        assert_eq!(LayerOrder::from_slice(&[0, 1, 2], 2), LayerOrder::new(2));
        // Üres count = 0 mellett érvényes.
        assert_eq!(LayerOrder::from_slice(&[], 0), LayerOrder::new(0));
    }
}
