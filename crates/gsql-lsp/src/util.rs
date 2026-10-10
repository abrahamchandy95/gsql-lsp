//! Small helpers shared across modules.

/// Pushes `item` unless `list` already holds an equal one.
pub(crate) fn push_unique<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// Appends the items of `more` that `list` does not hold yet, in order.
pub(crate) fn extend_unique<T: PartialEq>(
    list: &mut Vec<T>,
    more: impl IntoIterator<Item = T>,
) {
    for item in more {
        push_unique(list, item);
    }
}
