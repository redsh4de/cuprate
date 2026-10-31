//! # Traits
//!
//! Public and private traits for Pedersen hashing and tree building.
use std::{
    io,
    ops::Mul,
    sync::{Arc, LazyLock},
};

use ciphersuite::{
    Ciphersuite,
    group::{
        ff::{Field, PrimeField},
        prime::PrimeGroup,
    },
};
use curve25519_dalek::{Scalar, constants::ED25519_BASEPOINT_POINT, edwards::CompressedEdwardsY};
use dalek_ff_group::FieldElement;
use ec_divisors::DivisorCurve;
use helioselene::{Helios, Selene};
use monero_fcmp_plus_plus::{HELIOS_FCMP_GENERATORS, SELENE_FCMP_GENERATORS};
use monero_fcmp_plus_plus_generators::{HELIOS_HASH_INIT, SELENE_HASH_INIT};
use monero_oxide::ed25519::Point;
use rayon::prelude::*;

use cuprate_types::fcmp::{FcmpLeafAmount, FcmpLeafData};

use crate::Branch;

/// An internal trait for the curves used in Monero's FCMP++ tree.
pub(crate) trait FcmpCurve:
    Ciphersuite<
        F: PrimeField<Repr = [u8; 32]>,
        G: PrimeGroup<Repr = [u8; 32]>
               + DivisorCurve<FieldElement = <Self::Other as Ciphersuite>::F>,
    >
{
    /// The other curve that makes a curve cycle.
    type Other: FcmpCurve;

    /// The amount of children hashed to make a branch for this curve.
    const CHUNK_LEN: usize;

    /// The generators for Pedersen hashing.
    fn generators() -> &'static monero_fcmp_plus_plus_generators::FcmpGenerators<Self>;

    /// A fixed point to add to each Pedersen hash. `B_curve` in the FCMP paper.
    fn hash_init() -> Self::G;
}

/// A trait representing a child layer in the tree.
pub(crate) trait Child<P: FcmpCurve>: Clone + Sized + Send + Sync {
    /// The amount of parent scalars the child will make.
    const PARENT_SCALARS: usize;

    /// Converts the children into their parent scalars for hashing, in order.
    fn parent_scalars(children: &[Self]) -> Vec<P::F>;
}

/// A trait representing a reader for a child layer.
pub trait ChildReader<C>: Send + 'static {
    /// Get a batch of children from a given index. A size hint is given, but it does not need to be
    /// respected. If no children are returned it will be assumed the end of the child layer is reached.
    fn get_batch(&mut self, start_index: u64, batch_size_hint: u64) -> Result<Vec<C>, io::Error>;

    /// Get a specific child at the given index.
    fn get(&self, index: u64) -> Result<Option<C>, io::Error>;
}

/// A trait representing a writer to every layer of the tree, which commits them together.
pub trait TreeWriter: Send + 'static {
    /// The amount of branches in a layer, including the ones appended with this writer.
    fn branch_len(&self, layer_idx: usize) -> Result<u64, io::Error>;

    /// Get a batch of branches in a layer from a given index, including the ones appended with this writer.
    fn get_batch(
        &self,
        layer_idx: usize,
        start_index: u64,
        batch_size: u64,
    ) -> Result<Vec<Branch>, io::Error>;

    /// Append some new branches to a layer.
    fn append_branches(&mut self, layer_idx: usize, branches: &[Branch]) -> Result<(), io::Error>;

    /// Commit the new branches of every layer.
    fn commit(self) -> Result<(), io::Error>;
}

/// A trait representing a backing store for the FCMP tree.
pub trait TreeStore {
    type LayerReader: ChildReader<Branch>;
    type Writer: TreeWriter;

    fn layer_reader(&self, layer_idx: usize) -> Self::LayerReader;

    fn writer(&self) -> Self::Writer;
}

// impls

impl<T: TreeStore> TreeStore for Arc<T> {
    type LayerReader = T::LayerReader;
    type Writer = T::Writer;

    fn layer_reader(&self, layer_idx: usize) -> Self::LayerReader {
        T::layer_reader(self, layer_idx)
    }

    fn writer(&self) -> Self::Writer {
        T::writer(self)
    }
}

/// The new leaves type.
///
/// This is used for extending the FCMP tree with some new leaves to the right. Every given leaf must
/// be allowed to be inserted into the tree. i.e. no point should be the identity.
pub struct NewLeaves {
    pub leaves: Vec<FcmpLeafData>,
}

impl ChildReader<FcmpLeafData> for NewLeaves {
    fn get_batch(&mut self, _: u64, _: u64) -> Result<Vec<FcmpLeafData>, io::Error> {
        Ok(std::mem::take(&mut self.leaves))
    }

    fn get(&self, _: u64) -> Result<Option<FcmpLeafData>, io::Error> {
        unreachable!()
    }
}

impl FcmpCurve for Selene {
    type Other = Helios;

    const CHUNK_LEN: usize = 38;

    fn generators() -> &'static monero_fcmp_plus_plus_generators::FcmpGenerators<Self> {
        &SELENE_FCMP_GENERATORS
    }

    fn hash_init() -> Self::G {
        *SELENE_HASH_INIT
    }
}

impl FcmpCurve for Helios {
    type Other = Selene;

    const CHUNK_LEN: usize = 18;

    fn generators() -> &'static monero_fcmp_plus_plus_generators::FcmpGenerators<Self> {
        &HELIOS_FCMP_GENERATORS
    }

    fn hash_init() -> Self::G {
        *HELIOS_HASH_INIT
    }
}

impl Child<Selene> for FcmpLeafData {
    const PARENT_SCALARS: usize = 6;

    fn parent_scalars(leaves: &[Self]) -> Vec<<Selene as Ciphersuite>::F> {
        // A partial chunk is all the work there is when adding a block at the tip, so spread it
        // over threads.
        if leaves.len() > 4 && leaves.len() < Selene::CHUNK_LEN {
            return leaves
                .par_chunks(4)
                .flat_map_iter(<Self as Child<Selene>>::parent_scalars)
                .collect();
        }

        /// Ed25519's `d`, then the `A / 3` and `c` of the map to Wei25519 in `ec_divisors`' `to_xy`.
        static CONSTANTS: LazyLock<[FieldElement; 3]> = LazyLock::new(|| {
            let d = -FieldElement::from(121_665_u64)
                * FieldElement::from(121_666_u64).invert().unwrap();
            let a_div_3 =
                FieldElement::from(486_662_u64) * FieldElement::from(3_u64).invert().unwrap();

            // Take `c` from `to_xy` so its sign matches.
            let (_, wei_y) =
                DivisorCurve::to_xy(dalek_ff_group::EdwardsPoint(ED25519_BASEPOINT_POINT)).unwrap();
            let (y, x) = edwards_y_x(ED25519_BASEPOINT_POINT.compress().0, d);
            let c = wei_y * (FieldElement::ONE - y) * x * (FieldElement::ONE + y).invert().unwrap();

            [d, a_div_3, c]
        });

        /// Decodes a point to its Edwards `(y, x)` as monerod's `ge_frombytes_vartime` does.
        ///
        /// # Panics
        /// This function will panic if the point is not canonically encoded or not on the curve.
        fn edwards_y_x(mut bytes: [u8; 32], d: FieldElement) -> (FieldElement, FieldElement) {
            let x_is_odd = bytes[31] >> 7;
            bytes[31] &= u8::MAX >> 1;

            let y = FieldElement::from_repr(bytes).unwrap();
            let y_sq = y.square();
            let (is_square, mut x) =
                FieldElement::sqrt_ratio_i(y_sq - FieldElement::ONE, d * y_sq + FieldElement::ONE);
            assert!(bool::from(is_square));

            if x.is_odd().unwrap_u8() != x_is_odd {
                x = -x;
            }
            // `-0` is not canonical.
            assert_eq!(x.is_odd().unwrap_u8(), x_is_odd);

            (y, x)
        }

        /// The Edwards `(y, x)` of an on-chain point with its torsion cleared.
        fn torsion_cleared(bytes: [u8; 32], d: FieldElement) -> (FieldElement, FieldElement) {
            static INV_EIGHT: LazyLock<Scalar> = LazyLock::new(|| Scalar::from(8_u8).invert());

            let y_x = edwards_y_x(bytes, d);
            let point = CompressedEdwardsY(bytes).decompress().unwrap();

            if point.is_torsion_free() {
                y_x
            } else {
                edwards_y_x(point.mul_by_cofactor().mul(*INV_EIGHT).compress().0, d)
            }
        }

        let [d, a_div_3, wei_c] = *CONSTANTS;

        // An Ed25519 `(x, y)` is `((1 + y) / (1 - y) + A / 3, c (1 + y) / ((1 - y) x))` on Wei25519, so
        // collect the denominators to invert them all at once.
        let mut one_plus_y = Vec::with_capacity(leaves.len() * 3);
        let mut denominators = Vec::with_capacity(leaves.len() * 3);

        for leaf in leaves {
            let i = Point::biased_hash(leaf.o).into();

            let c = match leaf.c {
                // A clear amt, we can create a 0 commitment which we do not need to check for torsion
                FcmpLeafAmount::Clear(amt) => edwards_y_x(
                    cuprate_helper::crypto::compute_zero_commitment(amt)
                        .compress()
                        .to_bytes(),
                    d,
                ),
                // A commitment to an amt, check it for torsion.
                FcmpLeafAmount::Commitment(cmt) => torsion_cleared(cmt, d),
            };

            for (y, x) in [
                torsion_cleared(leaf.o, d),
                edwards_y_x(i.compress().0, d),
                c,
            ] {
                let one_minus_y = FieldElement::ONE - y;
                one_plus_y.push(FieldElement::ONE + y);
                denominators.push([one_minus_y, one_minus_y * x]);
            }
        }

        // Invert the product of all the denominators, then take each denominator back out of it.
        let denominators_flat = denominators.as_flattened_mut();
        let mut products = Vec::with_capacity(denominators_flat.len());
        let mut product = FieldElement::ONE;
        for denominator in &*denominators_flat {
            products.push(product);
            product *= denominator;
        }

        // Only the identity has a zero denominator.
        let mut inverse = product
            .invert()
            .expect("leaf points must not be the identity");
        for (denominator, product) in denominators_flat.iter_mut().zip(products).rev() {
            (*denominator, inverse) = (inverse * product, inverse * *denominator);
        }

        one_plus_y
            .iter()
            .zip(denominators)
            .flat_map(|(one_plus_y, [inverse, inverse_with_x])| {
                [
                    *one_plus_y * inverse + a_div_3,
                    wei_c * one_plus_y * inverse_with_x,
                ]
            })
            .collect()
    }
}

impl<P: FcmpCurve> Child<P> for Branch {
    const PARENT_SCALARS: usize = 1;

    fn parent_scalars(children: &[Self]) -> Vec<P::F> {
        #[inline(always)]
        const fn clear_sign(mut x: [u8; 32]) -> [u8; 32] {
            x[31] &= u8::MAX >> 1;
            x
        }

        children
            .iter()
            .map(|child| P::F::from_repr_vartime(clear_sign(*child)).unwrap())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use ciphersuite::group::Group;
    use curve25519_dalek::{EdwardsPoint, constants::EIGHT_TORSION};
    use rand::{RngCore, rngs::OsRng};

    use cuprate_helper::crypto::compute_zero_commitment;

    use super::*;

    /// The batched leaf conversion must match `to_xy` on each torsion cleared point.
    #[test]
    fn leaf_scalars_match_to_xy() {
        let decompress = |p: [u8; 32]| CompressedEdwardsY(p).decompress().unwrap();
        let cleared = |p: EdwardsPoint| p.mul_by_cofactor() * Scalar::from(8_u8).invert();
        let to_xy = |p: EdwardsPoint| {
            let (x, y) = DivisorCurve::to_xy(dalek_ff_group::EdwardsPoint(p)).unwrap();
            [x, y]
        };

        let leaves = (0..50)
            .map(|i| FcmpLeafData {
                o: (EdwardsPoint::random(&mut OsRng) + EIGHT_TORSION[i % 8])
                    .compress()
                    .0,
                c: if i % 2 == 0 {
                    FcmpLeafAmount::Clear(OsRng.next_u64())
                } else {
                    FcmpLeafAmount::Commitment(
                        (EdwardsPoint::random(&mut OsRng) + EIGHT_TORSION[i / 2 % 8])
                            .compress()
                            .0,
                    )
                },
            })
            .collect::<Vec<_>>();

        let expected = leaves
            .iter()
            .flat_map(|leaf| {
                let c = match leaf.c {
                    FcmpLeafAmount::Clear(amt) => {
                        compute_zero_commitment(amt).decompress().unwrap()
                    }
                    FcmpLeafAmount::Commitment(cmt) => cleared(decompress(cmt)),
                };

                [
                    to_xy(cleared(decompress(leaf.o))),
                    to_xy(Point::biased_hash(leaf.o).into()),
                    to_xy(c),
                ]
                .into_iter()
                .flatten()
            })
            .collect::<Vec<_>>();

        // A full batch, and a partial chunk which is split over threads.
        assert_eq!(
            <FcmpLeafData as Child<Selene>>::parent_scalars(&leaves),
            expected
        );
        assert_eq!(
            <FcmpLeafData as Child<Selene>>::parent_scalars(&leaves[..10]),
            expected[..60]
        );
    }
}
