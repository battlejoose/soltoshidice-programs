//! Solana's frame begins at a fixed address and is zeroed per invocation.
//! The stock descending allocator starts at the configured *maximum* address,
//! which faults immediately when an SDK callback uses the default 32 KiB frame.
//! Ascending allocation keeps small callbacks in that default frame. Application
//! transactions that need more memory must continue requesting 128 KiB.

/// Decode fixed-size on-chain arrays directly into heap storage. Borsh's
/// default Box<[T; N]> decoder first materializes the array on the SBF stack.
/// This preserves the fixed-array wire encoding (no Vec length prefix).
pub fn deserialize_fixed<T:anchor_lang::AnchorDeserialize,R:std::io::Read,const N:usize>(reader:&mut R)->std::io::Result<Box<[T;N]>> {
    let mut values=Vec::with_capacity(N);
    for _ in 0..N {values.push(T::deserialize_reader(reader)?);}
    values.into_boxed_slice().try_into().map_err(|_|std::io::Error::new(std::io::ErrorKind::InvalidData,"fixed array length mismatch"))
}
#[cfg(all(feature="custom-heap",target_os="solana"))]
pub struct AscendingHeap;

#[cfg(all(feature="custom-heap",target_os="solana"))]
unsafe impl std::alloc::GlobalAlloc for AscendingHeap {
    unsafe fn alloc(&self, layout:std::alloc::Layout)->*mut u8 {
        let start=anchor_lang::solana_program::entrypoint::HEAP_START_ADDRESS as usize;
        let cursor=start as *mut usize;
        let current=(*cursor).max(start+std::mem::size_of::<usize>());
        match allocation(current,layout.size(),layout.align(),start+128*1024) {
            Some((address,end))=>{*cursor=end;address as *mut u8},
            None=>std::ptr::null_mut(),
        }
    }
    unsafe fn dealloc(&self,_ptr:*mut u8,_layout:std::alloc::Layout) {}
}

#[cfg(any(test,all(feature="custom-heap",target_os="solana")))]
fn allocation(current:usize,size:usize,align:usize,limit:usize)->Option<(usize,usize)> {
    let address=current.checked_add(align-1)? & !(align-1);
    let end=address.checked_add(size)?;
    (end<=limit).then_some((address,end))
}

#[cfg(test)]
mod tests {
    use super::allocation;
    #[test]
    fn ascending_heap_preserves_alignment_and_rejects_overflow() {
        assert_eq!(allocation(8,1,8,32_768),Some((8,9)));
        assert_eq!(allocation(9,32,32,32_768),Some((32,64)));
        assert_eq!(allocation(32_760,8,8,32_768),Some((32_760,32_768)));
        assert_eq!(allocation(32_768,1,1,32_768),None);
        assert_eq!(allocation(usize::MAX-1,16,8,usize::MAX),None);
        assert_eq!(allocation(128,usize::MAX,8,usize::MAX),None);
    }
}
