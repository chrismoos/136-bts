use super::Party;
use std::ops::{Index, IndexMut};

pub(crate) struct PartySlots<T>([Option<T>; 2]);
impl<T> PartySlots<T> {
    pub(crate) fn pair(caller: T, callee: T) -> Self {
        Self([Some(caller), Some(callee)])
    }
    pub(crate) fn single(party: Party, value: T) -> Self {
        let mut slots = [None, None];
        slots[party.index()] = Some(value);
        Self(slots)
    }
    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        self.0.get(index)?.as_ref()
    }
    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.0.get_mut(index)?.as_mut()
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter().flatten()
    }
    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.0.iter_mut().flatten()
    }
    pub(crate) fn present(&self) -> [Option<Party>; 2] {
        [
            self.0[0].as_ref().map(|_| Party::Caller),
            self.0[1].as_ref().map(|_| Party::Callee),
        ]
    }
}
impl<T> Index<usize> for PartySlots<T> {
    type Output = T;
    fn index(&self, index: usize) -> &T {
        self.0[index]
            .as_ref()
            .expect("indexed party has a physical bearer")
    }
}
impl<T> IndexMut<usize> for PartySlots<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        self.0[index]
            .as_mut()
            .expect("indexed party has a physical bearer")
    }
}
impl<'a, T> IntoIterator for &'a mut PartySlots<T> {
    type Item = &'a mut T;
    type IntoIter = std::iter::Flatten<std::slice::IterMut<'a, Option<T>>>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut().flatten()
    }
}
impl<'a, T> IntoIterator for &'a PartySlots<T> {
    type Item = &'a T;
    type IntoIter = std::iter::Flatten<std::slice::Iter<'a, Option<T>>>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter().flatten()
    }
}
