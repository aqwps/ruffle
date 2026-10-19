use crate::descriptors::Descriptors;
use crate::globals::Globals;
use fnv::FnvHashMap;
use std::fmt::{Debug, Formatter};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

type PoolInner<T> = Mutex<PoolItems<T>>;
type Constructor<Type, Description> = Box<dyn Fn(&Descriptors, &Description) -> Type>;

#[derive(Debug, Default)]
pub struct TexturePool {
    pools: FnvHashMap<TextureKey, BufferPool<(wgpu::Texture, wgpu::TextureView), AlwaysCompatible>>,
    globals_cache: FnvHashMap<GlobalsKey, Arc<Globals>>,
    last_used: FnvHashMap<TextureKey, u64>,
    frame: u64,
    metrics: Arc<TexturePoolMetrics>,
}

/// CPU-owned pooled resources, not driver residency or persistent movie bitmaps.
#[derive(Debug, Default, Clone, Copy)]
pub struct TexturePoolStats {
    pub owned_bytes: u64,
    pub checked_out_bytes: u64,
    pub peak_checked_out_bytes: u64,
    pub created_textures: u64,
    pub created_bytes: u64,
    pub deferred_layers: u64,
    pub fallback_layers: u64,
}

#[derive(Debug, Default)]
struct TexturePoolMetrics {
    owned: AtomicU64,
    checked_out: AtomicU64,
    peak: AtomicU64,
    created: AtomicU64,
    created_bytes: AtomicU64,
    deferred_layers: AtomicU64,
    fallback_layers: AtomicU64,
}

#[derive(Clone)]
struct TextureAccounting {
    bytes: u64,
    metrics: Arc<TexturePoolMetrics>,
}

struct PoolItems<T> {
    items: Vec<T>,
    accounting: Option<TextureAccounting>,
}

impl<T> Deref for PoolItems<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl<T> DerefMut for PoolItems<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.items
    }
}

impl<T> Drop for PoolItems<T> {
    fn drop(&mut self) {
        if let Some(accounting) = &self.accounting {
            accounting.metrics.owned.fetch_sub(
                self.items.len() as u64 * accounting.bytes,
                Ordering::Relaxed,
            );
        }
    }
}

impl TexturePool {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn stats(&self) -> TexturePoolStats {
        TexturePoolStats {
            owned_bytes: self.metrics.owned.load(Ordering::Relaxed),
            checked_out_bytes: self.metrics.checked_out.load(Ordering::Relaxed),
            peak_checked_out_bytes: self.metrics.peak.load(Ordering::Relaxed),
            created_textures: self.metrics.created.load(Ordering::Relaxed),
            created_bytes: self.metrics.created_bytes.load(Ordering::Relaxed),
            deferred_layers: self.metrics.deferred_layers.load(Ordering::Relaxed),
            fallback_layers: self.metrics.fallback_layers.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn record_layer(&self, deferred: bool) {
        let counter = if deferred {
            &self.metrics.deferred_layers
        } else {
            &self.metrics.fallback_layers
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Retain recent idle resources within a shared budget. Checked-out entries
    /// remain valid; removing their pool merely prevents them returning later.
    pub fn finish_frame(&mut self, remaining_bytes: &mut u64) {
        self.frame = self.frame.wrapping_add(1);
        let mut keys: Vec<_> = self.pools.keys().copied().collect();
        keys.sort_unstable_by_key(|key| std::cmp::Reverse(self.last_used[key]));
        for key in keys {
            let pool = &self.pools[&key];
            let bytes = key.estimated_bytes();
            let recent = self.frame.wrapping_sub(self.last_used[&key]) <= 120;
            let any_idle = pool.retain_idle(|_| {
                if recent && bytes <= *remaining_bytes {
                    *remaining_bytes -= bytes;
                    true
                } else {
                    false
                }
            });
            if !any_idle {
                self.pools.remove(&key);
                self.last_used.remove(&key);
            }
        }
        if self.globals_cache.len() > 128 {
            self.globals_cache.clear();
        }
        self.metrics.created.store(0, Ordering::Relaxed);
        self.metrics.created_bytes.store(0, Ordering::Relaxed);
        self.metrics.deferred_layers.store(0, Ordering::Relaxed);
        self.metrics.fallback_layers.store(0, Ordering::Relaxed);
        self.metrics.peak.store(
            self.metrics.checked_out.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }

    pub fn get_texture(
        &mut self,
        descriptors: &Descriptors,
        size: wgpu::Extent3d,
        usage: wgpu::TextureUsages,
        format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> PoolEntry<(wgpu::Texture, wgpu::TextureView), AlwaysCompatible> {
        let key = TextureKey {
            size,
            usage,
            format,
            sample_count,
        };
        self.last_used.insert(key, self.frame);
        let accounting = TextureAccounting {
            bytes: key.estimated_bytes(),
            metrics: self.metrics.clone(),
        };
        let pool = self.pools.entry(key).or_insert_with(|| {
            // Preserve allocation identity even in release builds so a fatal
            // wgpu error can name the failed scratch target.
            let label = format!(
                "Scratch {}x{} {:?}, samples={}, usage={usage:?}",
                size.width, size.height, format, sample_count,
            );
            let mut pool = BufferPool::new(Box::new(move |descriptors, _description| {
                let texture = descriptors.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&label),
                    size,
                    mip_level_count: 1,
                    sample_count,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    view_formats: &[format],
                    usage,
                });
                let view = texture.create_view(&Default::default());
                (texture, view)
            }));
            pool.available
                .lock()
                .expect("Pool lock poisoned")
                .accounting = Some(accounting.clone());
            pool.accounting = Some(accounting);
            pool
        });
        pool.take(descriptors, AlwaysCompatible)
    }

    pub fn get_globals(
        &mut self,
        descriptors: &Descriptors,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Arc<Globals> {
        self.globals_cache
            .entry(GlobalsKey {
                viewport_width,
                viewport_height,
            })
            .or_insert_with(|| {
                Arc::new(Globals::new(
                    &descriptors.device,
                    &descriptors.bind_layouts.globals,
                    viewport_width,
                    viewport_height,
                ))
            })
            .clone()
    }
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
struct TextureKey {
    size: wgpu::Extent3d,
    usage: wgpu::TextureUsages,
    format: wgpu::TextureFormat,
    sample_count: u32,
}

impl TextureKey {
    fn estimated_bytes(&self) -> u64 {
        let (block_width, block_height) = self.format.block_dimensions();
        // Combined depth/stencil formats have no single copy aspect. Account
        // conservatively rather than silently treating all formats as RGBA8.
        let bytes = self.format.block_copy_size(None).unwrap_or(8);
        u64::from(self.size.width.div_ceil(block_width))
            .saturating_mul(u64::from(self.size.height.div_ceil(block_height)))
            .saturating_mul(u64::from(self.size.depth_or_array_layers))
            .saturating_mul(u64::from(self.sample_count))
            .saturating_mul(u64::from(bytes))
    }
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
struct GlobalsKey {
    viewport_width: u32,
    viewport_height: u32,
}

pub trait BufferDescription: Clone + Debug {
    type Cost: Ord;

    /// If the potential buffer represented by this description (`self`)
    /// fits another existing buffer and its description (`other`),
    /// return the cost to use that buffer instead of making a new one.
    ///
    /// Cost is an arbitrary unit, but lower is better.
    /// None means that the other buffer cannot be used in place of this one.
    fn cost_to_use(&self, other: &Self) -> Option<Self::Cost>;
}

#[derive(Clone, Debug)]
pub struct AlwaysCompatible;

impl BufferDescription for AlwaysCompatible {
    type Cost = ();

    fn cost_to_use(&self, _other: &Self) -> Option<()> {
        Some(())
    }
}

pub struct BufferPool<Type, Description: BufferDescription> {
    available: Arc<PoolInner<(Type, Description)>>,
    constructor: Constructor<Type, Description>,
    accounting: Option<TextureAccounting>,
}

impl<Type, Description: BufferDescription> Debug for BufferPool<Type, Description> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferPool").finish()
    }
}

impl<Type, Description: BufferDescription> BufferPool<Type, Description> {
    pub fn new(constructor: Constructor<Type, Description>) -> Self {
        Self {
            available: Arc::new(Mutex::new(PoolItems {
                items: vec![],
                accounting: None,
            })),
            constructor,
            accounting: None,
        }
    }

    fn retain_idle(&self, mut keep: impl FnMut(&Type) -> bool) -> bool {
        let mut available = self.available.lock().expect("Pool lock poisoned");
        let before = available.len();
        available.retain(|(item, _)| keep(item));
        if let Some(accounting) = &self.accounting {
            accounting.metrics.owned.fetch_sub(
                (before - available.len()) as u64 * accounting.bytes,
                Ordering::Relaxed,
            );
        }
        !available.is_empty()
    }

    pub fn take(
        &self,
        descriptors: &Descriptors,
        description: Description,
    ) -> PoolEntry<Type, Description> {
        let mut guard = self
            .available
            .lock()
            .expect("Should not be able to lock recursively");
        let mut best: Option<(Description::Cost, usize)> = None;
        for i in 0..guard.len() {
            if let Some(cost) = description.cost_to_use(&guard[i].1) {
                if let Some(best) = &mut best {
                    if best.0 > cost {
                        *best = (cost, i);
                    }
                } else if best.is_none() {
                    best = Some((cost, i));
                }
            }
        }

        let (item, used_description) = if let Some((_, best)) = best {
            guard.swap_remove(best)
        } else {
            let item = (self.constructor)(descriptors, &description);
            if let Some(accounting) = &self.accounting {
                accounting
                    .metrics
                    .owned
                    .fetch_add(accounting.bytes, Ordering::Relaxed);
                accounting.metrics.created.fetch_add(1, Ordering::Relaxed);
                accounting
                    .metrics
                    .created_bytes
                    .fetch_add(accounting.bytes, Ordering::Relaxed);
            }
            (item, description)
        };
        if let Some(accounting) = &self.accounting {
            let active = accounting
                .metrics
                .checked_out
                .fetch_add(accounting.bytes, Ordering::Relaxed)
                + accounting.bytes;
            accounting.metrics.peak.fetch_max(active, Ordering::Relaxed);
        }
        PoolEntry {
            item: Some(item),
            description: used_description,
            pool: Arc::downgrade(&self.available),
            accounting: self.accounting.clone(),
        }
    }
}

pub struct PoolEntry<Type, Description: BufferDescription> {
    item: Option<Type>,
    description: Description,
    pool: Weak<PoolInner<(Type, Description)>>,
    accounting: Option<TextureAccounting>,
}

impl<Type, Description: BufferDescription> Debug for PoolEntry<Type, Description>
where
    Type: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PoolEntry").field(&self.item).finish()
    }
}

impl<Type, Description: BufferDescription> Drop for PoolEntry<Type, Description> {
    fn drop(&mut self) {
        if let Some(item) = self.item.take() {
            if let Some(accounting) = &self.accounting {
                accounting
                    .metrics
                    .checked_out
                    .fetch_sub(accounting.bytes, Ordering::Relaxed);
            }
            if let Some(pool) = self.pool.upgrade() {
                pool.lock()
                    .expect("Should not be able to lock recursively")
                    .push((item, self.description.clone()));
            } else if let Some(accounting) = &self.accounting {
                accounting
                    .metrics
                    .owned
                    .fetch_sub(accounting.bytes, Ordering::Relaxed);
            }
        }
    }
}

impl<Type, Description: BufferDescription> Deref for PoolEntry<Type, Description> {
    type Target = Type;

    fn deref(&self) -> &Self::Target {
        self.item.as_ref().expect("Item should exist until dropped")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_estimate_respects_format_and_samples() {
        let mut key = TextureKey {
            size: wgpu::Extent3d {
                width: 2048,
                height: 1024,
                depth_or_array_layers: 1,
            },
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Rgba8Unorm,
            sample_count: 4,
        };
        assert_eq!(key.estimated_bytes(), 32 * 1024 * 1024);
        key.format = wgpu::TextureFormat::Stencil8;
        assert_eq!(key.estimated_bytes(), 8 * 1024 * 1024);
        key.sample_count = 1;
        assert_eq!(key.estimated_bytes(), 2 * 1024 * 1024);
    }

    #[test]
    fn idle_eviction_and_pool_drop_preserve_checked_out_resource() {
        let metrics = Arc::new(TexturePoolMetrics::default());
        let accounting = TextureAccounting {
            bytes: 10,
            metrics: metrics.clone(),
        };
        let mut pool: BufferPool<u32, AlwaysCompatible> =
            BufferPool::new(Box::new(|_, _| unreachable!()));
        pool.accounting = Some(accounting.clone());
        {
            let mut available = pool.available.lock().unwrap();
            available.accounting = Some(accounting.clone());
            available.push((1, AlwaysCompatible));
            available.push((2, AlwaysCompatible));
        }
        metrics.owned.store(30, Ordering::Relaxed);
        metrics.checked_out.store(10, Ordering::Relaxed);
        let active = PoolEntry {
            item: Some(3),
            description: AlwaysCompatible,
            pool: Arc::downgrade(&pool.available),
            accounting: Some(accounting),
        };
        assert!(!pool.retain_idle(|_| false));
        assert_eq!(metrics.owned.load(Ordering::Relaxed), 10);
        drop(pool);
        assert_eq!(*active, 3);
        assert_eq!(metrics.checked_out.load(Ordering::Relaxed), 10);
        drop(active);
        assert_eq!(metrics.owned.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.checked_out.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn returned_entries_are_accounted_when_storage_is_destroyed() {
        let metrics = Arc::new(TexturePoolMetrics::default());
        let accounting = TextureAccounting {
            bytes: 10,
            metrics: metrics.clone(),
        };
        let pool: BufferPool<u32, AlwaysCompatible> =
            BufferPool::new(Box::new(|_, _| unreachable!()));
        pool.available.lock().unwrap().accounting = Some(accounting.clone());
        metrics.owned.store(10, Ordering::Relaxed);
        metrics.checked_out.store(10, Ordering::Relaxed);
        let active = PoolEntry {
            item: Some(1),
            description: AlwaysCompatible,
            pool: Arc::downgrade(&pool.available),
            accounting: Some(accounting),
        };
        drop(active);
        assert_eq!(metrics.owned.load(Ordering::Relaxed), 10);
        assert_eq!(metrics.checked_out.load(Ordering::Relaxed), 0);
        drop(pool);
        assert_eq!(metrics.owned.load(Ordering::Relaxed), 0);
    }
}
