use crate::pairing::{
    CurveAffine,
    CurveProjective,
    Engine
};

use crate::pairing::ff::{
    PrimeField,
    Field,
    PrimeFieldRepr,
    ScalarEngine};

use std::sync::Arc;
use super::source::*;
use std::future::{Future};
use std::task::{Context, Poll};
use std::pin::{Pin};

extern crate futures;

use self::futures::future::{join_all, JoinAll};
use self::futures::executor::block_on;

use super::worker::{Worker, WorkerFuture};

use super::SynthesisError;

use cfg_if;

use std::ops::Range;


use std::cmp::Ordering;

fn quicksort_by_index_helper<T, F>(arr: &mut [T], left: isize, right: isize, compare: &F)
where F: Fn(usize, usize) -> Ordering {
    if right <= left {
        return
    }

    let mut i: isize = left - 1;
    let mut j: isize = right;
    let mut p: isize = i;
    let mut q: isize = j;
    let idx_v = right as usize;
    loop {
        i += 1;
        while compare(i as usize, idx_v) == Ordering::Less {
            i += 1
        }
        j -= 1;
        while compare(idx_v, j as usize) == Ordering::Less {
            if j == left {
                break
            }
            j -= 1;
        }
        if i >= j {
            break
        }
        arr.swap(i as usize, j as usize);
        if compare(i as usize, idx_v) == Ordering::Equal {
            p += 1;
            arr.swap(p as usize, i as usize)
        }
        if compare(idx_v, j as usize) == Ordering::Equal {
            q -= 1;
            arr.swap(j as usize, q as usize)
        }
    }

    arr.swap(i as usize, right as usize);
    j = i - 1;
    i += 1;
    let mut k: isize = left;
    while k < p {
        arr.swap(k as usize, j as usize);
        k += 1;
        j -= 1;
        assert!(k < arr.len() as isize);
    }
    k = right - 1;
    while k > q {
        arr.swap(i as usize, k as usize);
        k -= 1;
        i += 1;
        assert!(k != 0);
    }

    quicksort_by_index_helper(arr, left, j, compare);
    quicksort_by_index_helper(arr, i, right, compare);
}

pub fn quicksort_by_index<T, F>(arr: &mut [T], compare: F) where F: Fn(usize, usize) -> Ordering {
    if arr.len() <= 1 {
        return
    }

    let len = arr.len();
    quicksort_by_index_helper(arr, 0, (len - 1) as isize, &compare);
}


/// This genious piece of code works in the following way:
/// - choose `c` - the bit length of the region that one thread works on
/// - make `2^c - 1` buckets and initialize them with `G = infinity` (that's equivalent of zero)
/// - there is no bucket for "zero" cause it's not necessary
/// - go over the pairs `(base, scalar)`
/// - for each scalar calculate `scalar % 2^c` and add the base (without any multiplications!) to the 
/// corresponding bucket
/// - at the end each bucket will have an accumulated value that should be multiplied by the corresponding factor
/// between `1` and `2^c - 1` to get the right value
/// - here comes the first trick - you don't need to do multiplications at all, just add all the buckets together
/// starting from the first one `(a + b + c + ...)` and than add to the first sum another sum of the form
/// `(b + c + d + ...)`, and than the third one `(c + d + ...)`, that will result in the proper prefactor infront of every
/// accumulator, without any multiplication operations at all
/// - that's of course not enough, so spawn the next thread
/// - this thread works with the same bit width `c`, but SKIPS lowers bits completely, so it actually takes values
/// in the form `(scalar >> c) % 2^c`, so works on the next region
/// - spawn more threads until you exhaust all the bit length
/// - you will get roughly `[bitlength / c] + 1` inaccumulators
/// - double the highest accumulator enough times, add to the next one, double the result, add the next accumulator, continue
/// 
/// Demo why it works:
/// ```text
///     a * G + b * H = (a_2 * (2^c)^2 + a_1 * (2^c)^1 + a_0) * G + (b_2 * (2^c)^2 + b_1 * (2^c)^1 + b_0) * H
/// ```
/// - make buckets over `0` labeled coefficients
/// - make buckets over `1` labeled coefficients
/// - make buckets over `2` labeled coefficients
/// - accumulators over each set of buckets will have an implicit factor of `(2^c)^i`, so before summing thme up
/// "higher" accumulators must be doubled `c` times
///
#[cfg(not(feature = "nightly"))]
fn multiexp_inner<Q, D, G, S>(
    pool: &Worker,
    bases: S,
    density_map: D,
    exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
    skip: u32,
    c: u32,
    handle_trivial: bool
) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
    where for<'a> &'a Q: QueryDensity,
          D: Send + Sync + 'static + Clone + AsRef<Q>,
          G: CurveAffine,
          S: SourceBuilder<G>
{
    // Perform this region of the multiexp
    let this = {
        // let bases = bases.clone();
        // let exponents = exponents.clone();
        // let density_map = density_map.clone();

        // This is a Pippenger’s algorithm
        pool.compute(move || {
            // Accumulate the result
            let mut acc = G::Projective::zero();

            // Build a source for the bases
            let mut bases = bases.new();

            // Create buckets to place remainders s mod 2^c,
            // it will be 2^c - 1 buckets (no bucket for zeroes)

            // Create space for the buckets
            let mut buckets = vec![<G as CurveAffine>::Projective::zero(); (1 << c) - 1];

            let zero = <G::Engine as ScalarEngine>::Fr::zero().into_repr();
            let one = <G::Engine as ScalarEngine>::Fr::one().into_repr();

            // Sort the bases into buckets
            for (&exp, density) in exponents.iter().zip(density_map.as_ref().iter()) {
                // Go over density and exponents
                if density {
                    if exp == zero {
                        bases.skip(1)?;
                    } else if exp == one {
                        if handle_trivial {
                            bases.add_assign_mixed(&mut acc)?;
                        } else {
                            bases.skip(1)?;
                        }
                    } else {
                        // Place multiplication into the bucket: Separate s * P as 
                        // (s/2^c) * P + (s mod 2^c) P
                        // First multiplication is c bits less, so one can do it,
                        // sum results from different buckets and double it c times,
                        // then add with (s mod 2^c) P parts
                        let mut exp = exp;
                        exp.shr(skip);
                        let exp = exp.as_ref()[0] % (1 << c);

                        if exp != 0 {
                            bases.add_assign_mixed(&mut buckets[(exp - 1) as usize])?;
                        } else {
                            bases.skip(1)?;
                        }
                    }
                }
            }

            // Summation by parts
            // e.g. 3a + 2b + 1c = a +
            //                    (a) + b +
            //                    ((a) + b) + c
            let mut running_sum = G::Projective::zero();
            for exp in buckets.into_iter().rev() {
                running_sum.add_assign(&exp);
                acc.add_assign(&running_sum);
            }

            Ok(acc)
        })
    };

    this
}

#[cfg(not(feature = "nightly"))]
fn multiexp_dense_inner<G>(
    pool: &Worker,
    bases: Arc<Vec<G>>,
    exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
    skip: u32,
    c: u32,
    handle_trivial: bool
) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
    where G: CurveAffine
{
    // Perform this region of the multiexp
    let this = {
        // let bases = bases.clone();
        // let exponents = exponents.clone();
        // let density_map = density_map.clone();

        // This is a Pippenger’s algorithm
        pool.compute(move || {
            // Accumulate the result
            let mut acc = G::Projective::zero();

            // Create buckets to place remainders s mod 2^c,
            // it will be 2^c - 1 buckets (no bucket for zeroes)

            // Create space for the buckets
            let mut buckets = vec![<G as CurveAffine>::Projective::zero(); (1 << c) - 1];

            let zero = <G::Engine as ScalarEngine>::Fr::zero().into_repr();
            let one = <G::Engine as ScalarEngine>::Fr::one().into_repr();

            // Sort the bases into buckets
            for (&exp, base) in exponents.iter().zip(bases.iter()) {
                if exp == zero {
                    continue;
                } else if exp == one {
                    if handle_trivial {
                        acc.add_assign_mixed(&base);
                    } else {
                        continue;
                    }
                } else {
                    // Place multiplication into the bucket: Separate s * P as 
                    // (s/2^c) * P + (s mod 2^c) P
                    // First multiplication is c bits less, so one can do it,
                    // sum results from different buckets and double it c times,
                    // then add with (s mod 2^c) P parts
                    let mut exp = exp;
                    exp.shr(skip);
                    let exp = exp.as_ref()[0] % (1 << c);

                    if exp != 0 {
                        buckets[(exp - 1) as usize].add_assign_mixed(&base);
                    } else {
                        continue;
                    }
                }
            }

            // Summation by parts
            // e.g. 3a + 2b + 1c = a +
            //                    (a) + b +
            //                    ((a) + b) + c
            let mut running_sum = G::Projective::zero();
            for exp in buckets.into_iter().rev() {
                running_sum.add_assign(&exp);
                acc.add_assign(&running_sum);
            }

            Ok(acc)
        })
    };

    this
}

#[cfg(not(feature = "nightly"))]
fn affine_multiexp_inner<Q, D, G, S>(
    pool: &Worker,
    bases: S,
    density_map: D,
    exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
    skip: u32,
    c: u32,
    handle_trivial: bool
) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
    where for<'a> &'a Q: QueryDensity,
          D: Send + Sync + 'static + Clone + AsRef<Q>,
          G: CurveAffine,
          S: AccessableSourceBuilder<G>
{
    let reduction_size = 1 << 14;

    // Perform this region of the multiexp
    let this = {
        // let bases = bases.clone();
        // let exponents = exponents.clone();
        // let density_map = density_map.clone();

        // This is a Pippenger’s algorithm
        pool.compute(move || {
            // Accumulate the result
            let mut acc = G::Projective::zero();

            let mut work_size = 0usize;

            // Build a source for the bases
            let mut bases = bases.new();
            let mut work_sizes: Vec<usize> = vec![0; (1 << c) - 1];

            // let mut bucket_sums = vec![<G as CurveAffine>::Projective::zero(); (1 << c) - 1];

            let mut scratch_x_diff: Vec<Vec<G::Base>> = vec![Vec::with_capacity(reduction_size); (1 << c) - 1];
            let mut scratch_y_diff: Vec<Vec<G::Base>> = vec![Vec::with_capacity(reduction_size); (1 << c) - 1];
            let mut scratch_x0_x1_y0: Vec<Vec<(G::Base, G::Base, G::Base)>> = vec![Vec::with_capacity(reduction_size); (1 << c) - 1];

            // Create buckets to place remainders s mod 2^c,
            // it will be 2^c - 1 buckets (no bucket for zeroes)

            // Create space for the buckets
            let mut buckets: Vec<Vec<(G::Base, G::Base)>> = vec![Vec::with_capacity(reduction_size*2); (1 << c) - 1];

            let zero = <G::Engine as ScalarEngine>::Fr::zero().into_repr();
            let one = <G::Engine as ScalarEngine>::Fr::one().into_repr();

            // Sort the bases into buckets
            for (&exp, density) in exponents.iter().zip(density_map.as_ref().iter()) {
                // Go over density and exponents
                if density {
                    if exp == zero {
                        bases.skip(1)?;
                    } else if exp == one {
                        if handle_trivial {
                            buckets[0].push(bases.get_ref()?.into_xy_unchecked());
                            work_sizes[0] += 1;
                            if work_sizes[0] & 1 == 0 {
                                work_size += 1;
                            }
                        } else {
                            bases.skip(1)?;
                        }
                    } else {
                        // Place multiplication into the bucket: Separate s * P as 
                        // (s/2^c) * P + (s mod 2^c) P
                        // First multiplication is c bits less, so one can do it,
                        // sum results from different buckets and double it c times,
                        // then add with (s mod 2^c) P parts
                        let mut exp = exp;
                        exp.shr(skip);
                        let exp = exp.as_ref()[0] % (1 << c);

                        if exp != 0 {
                            buckets[(exp-1) as usize].push(bases.get_ref()?.into_xy_unchecked());
                            work_sizes[(exp-1) as usize] += 1;
                            if work_sizes[(exp-1) as usize] & 1 == 0 {
                                work_size += 1;
                            }

                        } else {
                            bases.skip(1)?;
                        }
                    }
                }

                if work_size >= reduction_size {
                    work_size = reduce::<G>(&mut buckets, &mut scratch_x_diff, &mut scratch_y_diff, &mut scratch_x0_x1_y0, &mut work_sizes)?;
                    // {
                    //     for ((bucket, running_sum_per_bucket), size) in buckets.iter_mut()
                    //                                         .zip(bucket_sums.iter_mut())
                    //                                         .zip(work_sizes.iter_mut()) {
                    //         let mut subsum = G::Projective::zero();
                    //         for _ in 0..bucket.len() {
                    //             running_sum_per_bucket.add_assign_mixed(&bucket.pop().unwrap());
                    //         }
                    //         *size = 0;
                    //     }
                    //     work_size = 0;
                    // }
                }
            }

            work_size = reduce::<G>(&mut buckets, &mut scratch_x_diff, &mut scratch_y_diff, &mut scratch_x0_x1_y0, &mut work_sizes)?;
            // {
                // for ((bucket, running_sum_per_bucket), size) in buckets.iter_mut()
                //                                     .zip(bucket_sums.iter_mut())
                //                                     .zip(work_sizes.iter_mut()) {
                //     let mut subsum = G::Projective::zero();
                //     for _ in 0..bucket.len() {
                //         running_sum_per_bucket.add_assign_mixed(&bucket.pop().unwrap());
                //     }
                //     *size = 0;
                // }
                // work_size = 0;
            // }


            // // Summation by parts
            // // e.g. 3a + 2b + 1c = a +
            // //                    (a) + b +
            // //                    ((a) + b) + c
            // let mut running_sum = G::Projective::zero();
            // for exp in bucket_sums.into_iter().rev() {
            //     running_sum.add_assign(&exp);
            //     acc.add_assign(&running_sum);
            // }

            // Summation by parts
            // e.g. 3a + 2b + 1c = a +
            //                    (a) + b +
            //                    ((a) + b) + c
            let mut running_sum = G::Projective::zero();
            for exp in buckets.into_iter().rev() {
                let mut subsum = G::Projective::zero();
                for b in exp.into_iter() {
                    let p = G::from_xy_unchecked(b.0, b.1);
                    subsum.add_assign_mixed(&p);
                }
                running_sum.add_assign(&subsum);
                acc.add_assign(&running_sum);
            }

            Ok(acc)
        })
    };

    this
}

#[cfg(not(feature = "nightly"))]
fn dense_affine_multiexp_inner<G>(
    pool: &Worker,
    bases: Arc<Vec<G>>,
    exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
    skip: u32,
    c: u32,
    handle_trivial: bool
) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
    where G: CurveAffine
{
    let reduction_size = 1 << 14;

    // Perform this region of the multiexp
    let this = {
        // let bases = bases.clone();
        // let exponents = exponents.clone();
        // let density_map = density_map.clone();

        // This is a Pippenger’s algorithm
        pool.compute(move || {
            // Accumulate the result
            let mut acc = G::Projective::zero();

            let mut work_size = 0usize;

            // Build a source for the bases
            let mut work_sizes: Vec<usize> = vec![0; (1 << c) - 1];

            // let mut bucket_sums = vec![<G as CurveAffine>::Projective::zero(); (1 << c) - 1];

            let mut scratch_x_diff: Vec<Vec<G::Base>> = vec![Vec::with_capacity(reduction_size); (1 << c) - 1];
            let mut scratch_y_diff: Vec<Vec<G::Base>> = vec![Vec::with_capacity(reduction_size); (1 << c) - 1];
            let mut scratch_x0_x1_y0: Vec<Vec<(G::Base, G::Base, G::Base)>> = vec![Vec::with_capacity(reduction_size); (1 << c) - 1];

            // Create buckets to place remainders s mod 2^c,
            // it will be 2^c - 1 buckets (no bucket for zeroes)

            // Create space for the buckets
            let mut buckets: Vec<Vec<(G::Base, G::Base)>> = vec![Vec::with_capacity(reduction_size*2); (1 << c) - 1];

            let zero = <G::Engine as ScalarEngine>::Fr::zero().into_repr();
            let one = <G::Engine as ScalarEngine>::Fr::one().into_repr();

            for (&exp, base) in exponents.iter().zip(bases.iter()) {
                if exp == zero {
                    continue
                } else if exp == one {
                    if handle_trivial {
                        buckets[0].push(base.into_xy_unchecked());
                        work_sizes[0] += 1;
                        if work_sizes[0] & 1 == 0 {
                            work_size += 1;
                        }
                    } 
                } else {
                    // Place multiplication into the bucket: Separate s * P as 
                    // (s/2^c) * P + (s mod 2^c) P
                    // First multiplication is c bits less, so one can do it,
                    // sum results from different buckets and double it c times,
                    // then add with (s mod 2^c) P parts
                    let mut exp = exp;
                    exp.shr(skip);
                    let exp = exp.as_ref()[0] % (1 << c);

                    if exp != 0 {
                        buckets[(exp-1) as usize].push(base.into_xy_unchecked());
                        work_sizes[(exp-1) as usize] += 1;
                        if work_sizes[(exp-1) as usize] & 1 == 0 {
                            work_size += 1;
                        }
                    }
                }

                if work_size >= reduction_size {
                    work_size = reduce::<G>(&mut buckets, &mut scratch_x_diff, &mut scratch_y_diff, &mut scratch_x0_x1_y0, &mut work_sizes)?;
                }
            }

            work_size = reduce::<G>(&mut buckets, &mut scratch_x_diff, &mut scratch_y_diff, &mut scratch_x0_x1_y0, &mut work_sizes)?;

            // // Summation by parts
            // // e.g. 3a + 2b + 1c = a +
            // //                    (a) + b +
            // //                    ((a) + b) + c
            // let mut running_sum = G::Projective::zero();
            // for exp in bucket_sums.into_iter().rev() {
            //     running_sum.add_assign(&exp);
            //     acc.add_assign(&running_sum);
            // }

            // Summation by parts
            // e.g. 3a + 2b + 1c = a +
            //                    (a) + b +
            //                    ((a) + b) + c
            let mut running_sum = G::Projective::zero();
            for exp in buckets.into_iter().rev() {
                let mut subsum = G::Projective::zero();
                for b in exp.into_iter() {
                    let p = G::from_xy_unchecked(b.0, b.1);
                    subsum.add_assign_mixed(&p);
                }
                running_sum.add_assign(&subsum);
                acc.add_assign(&running_sum);
            }

            Ok(acc)
        })
    };

    this
}

// #[cfg(not(feature = "nightly"))]
// fn dense_affine_multiexp_inner_by_ref<G>(
//     pool: &Worker,
//     bases: Arc<Vec<G>>,
//     exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
//     skip: u32,
//     c: u32,
//     handle_trivial: bool
// ) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
//     where G: CurveAffine
// {
//     let reduction_size = 1 << 14;

//     // Perform this region of the multiexp
//     let this = {
//         // let bases = bases.clone();
//         // let exponents = exponents.clone();
//         // let density_map = density_map.clone();

//         // This is a Pippenger’s algorithm
//         pool.compute(move || {
//             // Accumulate the result
//             let mut acc = G::Projective::zero();

//             let mut work_size = 0;
//             let num_buckets: usize = (1 << c) - 1;

//             use bit_vec::BitVec;

//             let mut chains_bitvec = BitVec::with_capacity(num_buckets);

//             let mut previous_chain_elem = 

//             let mut work_sizes: Vec<usize> = vec![0; num_buckets];

//             let mut chains: Vec<u64> = Vec::with_capacity(reduction_size);
//             let mut buckets: Vec<(G::Base, G::Base)> = Vec::with_capacity(reduction_size);

//             let mut chains_leftover_scratch: Vec<u64> = Vec::with_capacity(num_buckets);
//             let mut bases_leftover_scratch: Vec<(G::Base, G::Base)>  = Vec::with_capacity(num_buckets);

//             let mut scratch_prod: Vec<G::Base> = Vec::with_capacity(reduction_size/2);
//             let mut scratch_x_diff: Vec<G::Base> = Vec::with_capacity(reduction_size/2);
//             let mut scratch_y_diff: Vec<G::Base> = Vec::with_capacity(reduction_size/2);
//             let mut scratch_x0_x1_y0: Vec<(G::Base, G::Base, G::Base)> = Vec::with_capacity(reduction_size/2);

//             // Create buckets to place remainders s mod 2^c,
//             // it will be 2^c - 1 buckets (no bucket for zeroes)

//             let zero = <G::Engine as ScalarEngine>::Fr::zero().into_repr();
//             let one = <G::Engine as ScalarEngine>::Fr::one().into_repr();

//             for (&exp, base) in exponents.iter().zip(bases.iter()) {
//                 if exp == zero {
//                     continue
//                 } else if exp == one {
//                     if handle_trivial {
//                         let index = encode_bucket(0, buckets.len());
//                         chains.push(index);
//                         buckets.push(base.into_xy_unchecked());
//                         work_sizes[0] += 1;
//                         if work_sizes[0] & 1 == 0 {
//                             work_size += 1;
//                         }
//                     } 
//                 } else {
//                     // Place multiplication into the bucket: Separate s * P as 
//                     // (s/2^c) * P + (s mod 2^c) P
//                     // First multiplication is c bits less, so one can do it,
//                     // sum results from different buckets and double it c times,
//                     // then add with (s mod 2^c) P parts
//                     let mut exp = exp;
//                     exp.shr(skip);
//                     let exp = exp.as_ref()[0] % (1 << c);

//                     if exp != 0 {
//                         let index = encode_bucket((exp-1) as usize, buckets.len());
//                         chains.push(index);
//                         buckets.push(base.into_xy_unchecked());
//                         work_sizes[(exp-1) as usize] += 1;
//                         if work_sizes[(exp-1) as usize] & 1 == 0 {
//                             work_size += 1;
//                         }
//                     }
//                 }

//                 if chains.len() >= reduction_size {
//                     work_size = reduce_by_ref::<G>(&mut buckets, &mut chains, &mut scratch_x_diff, &mut scratch_y_diff, &mut scratch_x0_x1_y0, &mut scratch_prod, &mut bases_leftover_scratch, &mut chains_leftover_scratch, &mut work_sizes)?;
//                 }
//             }

//             // let threshold = 1 << 10;

//             // while chains.len() > threshold {
//                 work_size = reduce_by_ref::<G>(&mut buckets, &mut chains, &mut scratch_x_diff, &mut scratch_y_diff, &mut scratch_x0_x1_y0, &mut scratch_prod, &mut bases_leftover_scratch, &mut chains_leftover_scratch, &mut work_sizes)?;
//             // }

//             // let bucket_index_mask :u64 = (1u64 << 32) - 1;

//             quicksort_by_index(&mut buckets, |i, j| {
//                 (decode_bucket(chains[i]).0).cmp(&decode_bucket(chains[j]).0)
//                 // (chains[i] & bucket_index_mask).cmp(&(chains[j] & bucket_index_mask))
//             });

//             let mut running_sum = G::Projective::zero();
//             let mut buckets_rev_iter = buckets.into_iter().rev();

//             for work_size in work_sizes.into_iter().rev() {
//                 let mut subsum = G::Projective::zero();
//                 for _ in 0..work_size {
//                     let (x, y) = buckets_rev_iter.next().unwrap();
//                     let p = G::from_xy_unchecked(x, y);
//                     subsum.add_assign_mixed(&p);
//                 }
//                 running_sum.add_assign(&subsum);
//                 acc.add_assign(&running_sum);
//             }

//             // // Summation by parts
//             // // e.g. 3a + 2b + 1c = a +
//             // //                    (a) + b +
//             // //                    ((a) + b) + c
//             // let mut running_sum = G::Projective::zero();
//             // for exp in bucket_sums.into_iter().rev() {
//             //     running_sum.add_assign(&exp);
//             //     acc.add_assign(&running_sum);
//             // }

//             // Summation by parts
//             // e.g. 3a + 2b + 1c = a +
//             //                    (a) + b +
//             //                    ((a) + b) + c

//             // let mut running_sum = G::Projective::zero();
//             // for exp in buckets.into_iter().rev() {
//             //     let mut subsum = G::Projective::zero();
//             //     for b in exp.into_iter() {
//             //         let p = G::from_xy_unchecked(b.0, b.1);
//             //         subsum.add_assign_mixed(&p);
//             //     }
//             //     running_sum.add_assign(&subsum);
//             //     acc.add_assign(&running_sum);
//             // }

//             Ok(acc)
//         })
//     };

//     this
// }


#[cfg(not(feature = "nightly"))]
fn dense_affine_multiexp_inner_by_ref<G>(
    pool: &Worker,
    bases: Arc<Vec<G>>,
    exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
    skip: u32,
    c: u32,
    handle_trivial: bool
) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
    where G: CurveAffine
{
    let reduction_size = 1 << 16;
    let reduction_threshold = 1 << 6;

    // Perform this region of the multiexp
    let this = {
        // This is a Pippenger’s algorithm
        pool.compute(move || {
            // Accumulate the result
            let mut acc = G::Projective::zero();

            let mut work_size = 0;
            let num_buckets: usize = (1 << c) - 1;

            use bit_vec::BitVec;

            let zero_placeholder = G::zero();

            let mut points_strage_scratch = vec![zero_placeholder; reduction_size];

            let mut chains_bitvec = BitVec::from_elem(num_buckets, false);
            let mut previous_chain_elem: Vec<&G> = vec![&zero_placeholder; num_buckets];

            // bucket, (x2, x1, y2, y1)
            let mut accumulator: Vec<(usize, PointPairIndex<G>)> = Vec::with_capacity(reduction_size);

            let mut initial_schedule_scratch: Vec<usize> = vec![0; num_buckets];
            let mut this_round_schedule_scratch: Vec<usize> = vec![0; num_buckets];
            let mut next_round_schedule_scratch: Vec<usize> = vec![0; num_buckets];

            let mut scratch_prod: Vec<G::Base> = Vec::with_capacity(reduction_size);
            let mut scratch_x_diff: Vec<G::Base> = Vec::with_capacity(reduction_size);
            let mut scratch_final_reduction: Vec<Range<usize>> = Vec::with_capacity(reduction_threshold);

            // Create buckets to place remainders s mod 2^c,
            // it will be 2^c - 1 buckets (no bucket for zeroes)

            let zero = <G::Engine as ScalarEngine>::Fr::zero().into_repr();
            let one = <G::Engine as ScalarEngine>::Fr::one().into_repr();

            // let mut start = std::time::Instant::now();

            for (&exp, base) in exponents.iter().zip(bases.iter()) {
                if exp == zero {
                    continue
                } else if exp == one {
                    if handle_trivial {
                        if chains_bitvec.get(0).unwrap() {
                            chains_bitvec.set(0, false);
                            let tmp = previous_chain_elem[0];
                            accumulator.push((0, PointPairIndex::Reference([base, tmp])));
                        } else {
                            chains_bitvec.set(0, true);
                            previous_chain_elem[0] = base;
                        }
                    } 
                } else {
                    // Place multiplication into the bucket: Separate s * P as 
                    // (s/2^c) * P + (s mod 2^c) P
                    // First multiplication is c bits less, so one can do it,
                    // sum results from different buckets and double it c times,
                    // then add with (s mod 2^c) P parts
                    let mut exp = exp;
                    exp.shr(skip);
                    let exp = exp.as_ref()[0] % (1 << c);

                    if exp != 0 {
                        let bucket_index = (exp-1) as usize;
                        if chains_bitvec.get(bucket_index).unwrap() {
                            chains_bitvec.set(bucket_index, false);
                            let tmp = previous_chain_elem[bucket_index];
                            accumulator.push((bucket_index, PointPairIndex::Reference([base, tmp])));
                        } else {
                            chains_bitvec.set(bucket_index, true);
                            previous_chain_elem[bucket_index] = base;
                        }
                    }
                }

                if accumulator.len() >= reduction_size {
                    // println!("Placement taken {:?}", start.elapsed());
                    // start = std::time::Instant::now();
                    reduce_by_ref::<G>(
                        &mut accumulator, 
                        &mut scratch_x_diff, 
                        &mut scratch_prod,
                        &mut initial_schedule_scratch,
                        &mut this_round_schedule_scratch,
                        &mut next_round_schedule_scratch,
                        num_buckets,
                        reduction_threshold,
                        &mut acc,
                        &mut points_strage_scratch,
                        &mut scratch_final_reduction,
                    )?;

                    // println!("Reduction taken {:?}", start.elapsed());
                    // start = std::time::Instant::now();
                }
            }

            reduce_by_ref::<G>(
                &mut accumulator, 
                &mut scratch_x_diff, 
                &mut scratch_prod,
                &mut initial_schedule_scratch,
                &mut this_round_schedule_scratch,
                &mut next_round_schedule_scratch,
                num_buckets,
                reduction_threshold,
                &mut acc,
                &mut points_strage_scratch,
                &mut scratch_final_reduction,
            )?;

            // let mut running_sum = G::Projective::zero();
            // let mut buckets_rev_iter = buckets.into_iter().rev();

            // for work_size in work_sizes.into_iter().rev() {
            //     let mut subsum = G::Projective::zero();
            //     for _ in 0..work_size {
            //         let (x, y) = buckets_rev_iter.next().unwrap();
            //         let p = G::from_xy_unchecked(x, y);
            //         subsum.add_assign_mixed(&p);
            //     }
            //     running_sum.add_assign(&subsum);
            //     acc.add_assign(&running_sum);
            // }

            // // Summation by parts
            // // e.g. 3a + 2b + 1c = a +
            // //                    (a) + b +
            // //                    ((a) + b) + c
            // let mut running_sum = G::Projective::zero();
            // for exp in bucket_sums.into_iter().rev() {
            //     running_sum.add_assign(&exp);
            //     acc.add_assign(&running_sum);
            // }

            // Summation by parts
            // e.g. 3a + 2b + 1c = a +
            //                    (a) + b +
            //                    ((a) + b) + c

            // let mut running_sum = G::Projective::zero();
            // for exp in buckets.into_iter().rev() {
            //     let mut subsum = G::Projective::zero();
            //     for b in exp.into_iter() {
            //         let p = G::from_xy_unchecked(b.0, b.1);
            //         subsum.add_assign_mixed(&p);
            //     }
            //     running_sum.add_assign(&subsum);
            //     acc.add_assign(&running_sum);
            // }

            Ok(acc)
        })
    };

    this
}

fn decode_bucket(encoding: u64) -> (usize, usize) {
    let bucket_index = (encoding >> 32) as usize;
    let reference_index = (encoding as u32) as usize;

    (bucket_index, reference_index)
}

fn encode_bucket(bucket_index: usize, reference_index: usize) -> u64 {
    let encoding = ((bucket_index << 32) as u64) | (reference_index as u64);

    encoding
}

fn total_len<G: CurveAffine>(buckets: &Vec<Vec<(G::Base, G::Base)>>) -> usize {
    let mut result = 0;
    for b in buckets.iter() {
        result += b.len();
    }

    result
}

enum PointPairIndex<'a, G: CurveAffine> {
    Reference([&'a G; 2]),
    Index([usize; 2])
}

fn reduce<G: CurveAffine>(
    buckets: &mut Vec<Vec<(G::Base, G::Base)>>, 
    scratch_pad_x_diff: &mut Vec<Vec<G::Base>>,
    scratch_pad_y_diff: &mut Vec<Vec<G::Base>>, 
    scratch_pad_x0_x1_y0: &mut Vec<Vec<(G::Base, G::Base, G::Base)>>,
    work_counters: &mut Vec<usize>
) -> Result<usize, SynthesisError> {
    let initial_size = total_len::<G>(&*buckets);

    // First pass: compute [a, ab, abc, ...]
    let mut prod = Vec::with_capacity(initial_size);

    let one = <G::Base as Field>::one();
    let mut tmp = one;

    for ((((b, scratch_y), scratch_x), scratch_x0_x1_y0), work_counter) in buckets.iter_mut()
                                    .zip(scratch_pad_y_diff.iter_mut())
                                    .zip(scratch_pad_x_diff.iter_mut())
                                    .zip(scratch_pad_x0_x1_y0.iter_mut())
                                    .zip(work_counters.iter()) {
        assert!(scratch_x.len() == 0);
        assert!(scratch_y.len() == 0);
        assert!(scratch_x0_x1_y0.len() == 0);

        let len = b.len();

        assert!(*work_counter == len);

        if len <= 1 {
            continue;
        }

        // println!("During merging bucket len = {}", len);

        // make windows of two
        let mut drain_iter = if len & 1 == 1 {
            b.drain(1..)
        } else {
            b.drain(0..)
        };

        // let mut iter = b.into_iter();
        for _ in 0..(len/2) {
            let (x0, y0) = drain_iter.next().unwrap();
            let (x1, y1) = drain_iter.next().unwrap();

            let mut y_diff = y1;
            y_diff.sub_assign(&y0);

            let mut x_diff = x1;
            x_diff.sub_assign(&x0);

            scratch_y.push(y_diff);
            scratch_x.push(x_diff);
            scratch_x0_x1_y0.push((x0, x1, y0));

            tmp.mul_assign(&x_diff);
            prod.push(tmp);

        }
    }

    tmp = tmp.inverse().unwrap();

    
    // [abcd..., ..., ab, a, 1]
    // this is just a batch inversion
    let mut prod_iter = prod.into_iter().rev().skip(1).chain(Some(one));

    for x_diff in scratch_pad_x_diff.iter_mut().rev() {
        for x_diff_value in x_diff.iter_mut().rev() {
            let p = prod_iter.next().unwrap();
            let mut newtmp = tmp;
            newtmp.mul_assign(&x_diff_value);
            *x_diff_value = tmp;
            x_diff_value.mul_assign(&p);
    
            tmp = newtmp;
        }
    }

    // assert!(prod_iter.next().is_none());

    for ((((b, scratch_y), scratch_x), scratch_x0_x1_y0), work_counter) in buckets.iter_mut()
                                        .zip(scratch_pad_y_diff.iter_mut())
                                        .zip(scratch_pad_x_diff.iter_mut())
                                        .zip(scratch_pad_x0_x1_y0.iter_mut())
                                        .zip(work_counters.iter_mut()) {
        if *work_counter <= 1 {
            continue;
        }

        // println!("During work bucket len = {}", b.len());

        for ((x_diff, y_diff), x0_x1_y0) in scratch_x.iter().zip(scratch_y.iter()).zip(scratch_x0_x1_y0.iter()) {
            let mut lambda = *y_diff;
            lambda.mul_assign(&x_diff);

            let mut x_new = lambda;
            x_new.square();

            x_new.sub_assign(&x0_x1_y0.0); // - x0
            x_new.sub_assign(&x0_x1_y0.1); // - x1

            let mut y_new = x0_x1_y0.1; // x1
            y_new.sub_assign(&x_new);
            y_new.mul_assign(&lambda);
            y_new.sub_assign(&x0_x1_y0.2);

            b.push((x_new, y_new));
        }


        *work_counter = b.len();

        scratch_x.truncate(0);
        scratch_y.truncate(0);
        scratch_x0_x1_y0.truncate(0);
    }

    let final_size = total_len::<G>(&*buckets);

    assert!(initial_size >= final_size, "initial size is {}, final is {}", initial_size, final_size);

    Ok(final_size)
}

fn reduce_by_ref<'a, G: CurveAffine>(
    accumulator: &mut Vec<(usize, PointPairIndex<'a, G>)>,
    scratch_pad_x_diff: &mut Vec<G::Base>,
    prod_scratch: &mut Vec<G::Base>,
    initial_schedule_scratch: &mut Vec<usize>,
    this_round_schedule_scratch: &mut Vec<usize>,
    next_round_schedule_scratch: &mut Vec<usize>,
    num_buckets: usize,
    threshold: usize,
    sum_accumulator: &mut G::Projective,
    points_storage: &mut Vec<G>,
    ranges_scratch: &mut Vec<Range<usize>>,
) -> Result<(), SynthesisError> {

    let one = <G::Base as Field>::one();
    let mut tmp = one; 

    // let start = std::time::Instant::now();

    accumulator.sort_by(|a, b| a.0.cmp(&b.0));
    // println!("Sorting taken {:?}", start.elapsed());

    let mut current_bucket_index = 0;

    initial_schedule_scratch.truncate(0);
    initial_schedule_scratch.resize(num_buckets, 0);
    // this_round_schedule_scratch.truncate(0);

    next_round_schedule_scratch.truncate(0);
    next_round_schedule_scratch.resize(num_buckets, 0);

    for pair in accumulator.iter() {
        let (bucket, _) = pair;
        if *bucket != current_bucket_index {
            current_bucket_index = *bucket;
        }

        initial_schedule_scratch[current_bucket_index] += 1;
    }

    // println!("Initial buckets schedule is {:?}", initial_schedule_scratch);

    this_round_schedule_scratch.copy_from_slice(&initial_schedule_scratch);

    // now we know the initial schedule and can base on it

    let mut round_number = 0;

    loop {
        let pair_iteration_step_size = 1 << round_number; 

        let mut true_sums_to_perform = 0;
        for &num_pairs in this_round_schedule_scratch.iter() {
            if num_pairs == 0 {
                continue
            } else if num_pairs == 1 {
                continue
            } else {
                true_sums_to_perform += num_pairs;
            }
        }

        // let total_sums_to_perform: usize = this_round_schedule_scratch.iter().sum();

        if true_sums_to_perform < threshold {
            let mut shift = 0;
            for (bucket_idx, &bucket_num_pairs) in this_round_schedule_scratch.iter().enumerate() {
                if bucket_num_pairs == 0 {
                    continue
                }
    
                let initial_num_pairs = initial_schedule_scratch[bucket_idx];
    
                let range = shift..(bucket_num_pairs + shift);
    
                shift += initial_num_pairs;

                ranges_scratch.push(range);
            }

            let drain = ranges_scratch.drain(0..ranges_scratch.len());
            let mut running_sum = G::Projective::zero();
            for range in drain.into_iter().rev() {
                let mut subsum = G::Projective::zero();
                for i in range {
                    match accumulator[i].1 {
                        PointPairIndex::Reference([p1, p0]) => {
                            subsum.add_assign_mixed(&p1);
                            subsum.add_assign_mixed(&p0);
                        },
                        PointPairIndex::Index([idx_1, idx_0]) => {
                            let (x1, y1) = points_storage[idx_1].into_xy_unchecked();
                            let (x0, y0) = points_storage[idx_0].into_xy_unchecked();

                            subsum.add_assign_mixed(&G::from_xy_unchecked(x1, y1));
                            subsum.add_assign_mixed(&G::from_xy_unchecked(x0, y0));
                        }
                    }
                }

                running_sum.add_assign(&subsum);
                sum_accumulator.add_assign(&running_sum);
            }

            ranges_scratch.truncate(0);
            accumulator.truncate(0);
            break;
        }

        for (i, num_pairs) in this_round_schedule_scratch.iter().enumerate() {
            next_round_schedule_scratch[i] = (num_pairs / 2) + (num_pairs % 2);
        }

        // let mut actual_pairs = 0;

        let mut shift = 0;
        for (bucket_idx, &bucket_num_pairs) in this_round_schedule_scratch.iter().enumerate() {
            if bucket_num_pairs == 0 {
                continue
            } else if bucket_num_pairs == 1 {
                // we do not perform addition on one pair and instead we postpone it
                shift += initial_schedule_scratch[bucket_idx];
                continue
            };

            let initial_num_pairs = initial_schedule_scratch[bucket_idx];

            // we are always writing into the begining, so we use current number of elements in a bucket

            let range = if bucket_num_pairs & 1 == 1 {
                (shift+1)..(bucket_num_pairs + shift)
            } else {
                shift..(bucket_num_pairs + shift)
            };

            shift += initial_num_pairs;

            for i in range {
                let (x1, x0) = match accumulator[i].1 {
                    PointPairIndex::Reference([p1, p0]) => {
                        let (x1, _) = p1.into_xy_unchecked();
                        let (x0, _) = p0.into_xy_unchecked();

                        (x1, x0)
                    },
                    PointPairIndex::Index([idx_1, idx_0]) => {
                        let (x1, _) = points_storage[idx_1].into_xy_unchecked();
                        let (x0, _) = points_storage[idx_0].into_xy_unchecked();
                        
                        (x1, x0)
                    }
                };

                let mut x_diff = x1;
                x_diff.sub_assign(&x0);

                tmp.mul_assign(&x_diff);
                prod_scratch.push(tmp);
                scratch_pad_x_diff.push(x_diff);

                // actual_pairs += 1;
            }
        }

        // println!("Summing {} pairs", actual_pairs);

        // First pass: compute [a, ab, abc, ...]

        tmp = tmp.inverse().ok_or(SynthesisError::DivisionByZero)?;

        // [abcd..., ..., ab, a, 1]
        // this is just a batch inversion
        let prod_iter = prod_scratch.drain(0..prod_scratch.len()).rev().skip(1).chain(Some(one));

        for (x_diff_value, p) in scratch_pad_x_diff.iter_mut().rev().zip(prod_iter) {
            let mut newtmp = tmp;
            newtmp.mul_assign(&x_diff_value);
            *x_diff_value = tmp;
            x_diff_value.mul_assign(&p);

            tmp = newtmp;
        }

        let mut scratch_pad_x_diff_drain = scratch_pad_x_diff.drain(0..scratch_pad_x_diff.len());

        // let mut summed_pairs = 0;

        let mut shift = 0;
        let mut global_points_storage_index_to_use = 0;
        for (bucket_idx, &bucket_num_pairs) in this_round_schedule_scratch.iter().enumerate() {
            if bucket_num_pairs == 0 {
                continue
            } else if bucket_num_pairs == 1 {
                // we do not perform addition on one pair and instead we postpone it
                shift += initial_schedule_scratch[bucket_idx];
                continue
            };

            let initial_num_pairs = initial_schedule_scratch[bucket_idx];

            let range = if bucket_num_pairs & 1 == 1 {
                (shift+1)..(bucket_num_pairs + shift)
            } else {
                shift..(bucket_num_pairs + shift)
            };

            let base_idx_to_write = if bucket_num_pairs & 1 == 1 {
                shift+1
            } else {
                shift
            };

            debug_assert!(range.len() % 2 == 0);

            let num_steps = range.len() / 2;

            let mut it = range.into_iter();

            for idx_to_write in 0..num_steps { 
                let idx_0 = it.next().unwrap();
                let idx_1 = it.next().unwrap();

                let sum_idx_0 = {
                    let idx = idx_0;
                    let (x1, y1, x0, y0) = match accumulator[idx].1 {
                        PointPairIndex::Reference([p1, p0]) => {
                            let (x1, y1) = p1.into_xy_unchecked();
                            let (x0, y0) = p0.into_xy_unchecked();

                            (x1, y1, x0, y0)
                        },
                        PointPairIndex::Index([idx_1, idx_0]) => {
                            let (x1, y1) = points_storage[idx_1].into_xy_unchecked();
                            let (x0, y0) = points_storage[idx_0].into_xy_unchecked();
                            
                            (x1, y1, x0, y0)
                        }
                    };

                    // debug_assert_eq!(bucket_idx, bucket);

                    let mut x0_plus_x1 = x0;
                    x0_plus_x1.add_assign(&x1);

                    let mut lambda = y1;
                    lambda.sub_assign(&y0);
                    lambda.mul_assign(&scratch_pad_x_diff_drain.next().expect("must take an inverse"));

                    let mut x_new = lambda;
                    x_new.square();
                    x_new.sub_assign(&x0_plus_x1);

                    let mut y_new = x1; // x1
                    y_new.sub_assign(&x_new);
                    y_new.mul_assign(&lambda);
                    y_new.sub_assign(&y0);

                    let p = G::from_xy_unchecked(x_new, y_new);
                    points_storage[global_points_storage_index_to_use] = p;
                    let ref_to_point = global_points_storage_index_to_use;
                    global_points_storage_index_to_use += 1;

                    ref_to_point
                };

                let sum_idx_1 = {
                    let idx = idx_1;
                    let (x1, y1, x0, y0) = match accumulator[idx].1 {
                        PointPairIndex::Reference([p1, p0]) => {
                            let (x1, y1) = p1.into_xy_unchecked();
                            let (x0, y0) = p0.into_xy_unchecked();

                            (x1, y1, x0, y0)
                        },
                        PointPairIndex::Index([idx_1, idx_0]) => {
                            let (x1, y1) = points_storage[idx_1].into_xy_unchecked();
                            let (x0, y0) = points_storage[idx_0].into_xy_unchecked();
                            
                            (x1, y1, x0, y0)
                        }
                    };

                    // debug_assert_eq!(bucket_idx, bucket);

                    let mut x0_plus_x1 = x0;
                    x0_plus_x1.add_assign(&x1);

                    let mut lambda = y1;
                    lambda.sub_assign(&y0);
                    lambda.mul_assign(&scratch_pad_x_diff_drain.next().expect("must take an inverse"));

                    let mut x_new = lambda;
                    x_new.square();
                    x_new.sub_assign(&x0_plus_x1);

                    let mut y_new = x1; // x1
                    y_new.sub_assign(&x_new);
                    y_new.mul_assign(&lambda);
                    y_new.sub_assign(&y0);

                    let p = G::from_xy_unchecked(x_new, y_new);
                    points_storage[global_points_storage_index_to_use] = p;
                    let ref_to_point = global_points_storage_index_to_use;
                    global_points_storage_index_to_use += 1;

                    ref_to_point
                };

                let new_pair = (bucket_idx, PointPairIndex::Index([sum_idx_1, sum_idx_0]));
                // we always write contiguosly into the first available bucket!
                let write_to = idx_to_write + base_idx_to_write;
                // println!("Writing bucket {} into {}", bucket_idx, write_to);
                accumulator[write_to] = new_pair;
            }

            shift += initial_num_pairs;
        }

        // println!("Summed {}", summed_pairs);

        // this_round_schedule_scratch.truncate(0);
        this_round_schedule_scratch.copy_from_slice(&next_round_schedule_scratch);

        next_round_schedule_scratch.truncate(0);
        next_round_schedule_scratch.resize(num_buckets, 0);

        round_number += 1;
    }

    Ok(())
}

// fn reduce_by_ref<G: CurveAffine>(
//     buckets: &mut Vec<(G::Base, G::Base)>,
//     chains: &mut Vec<u64>, 
//     scratch_pad_x_diff: &mut Vec<G::Base>,
//     scratch_pad_y_diff: &mut Vec<G::Base>, 
//     scratch_pad_x0_x1_y0: &mut Vec<(G::Base, G::Base, G::Base)>,
//     prod_scratch: &mut Vec<G::Base>,
//     leftover_buckets_scratch: &mut Vec<(G::Base, G::Base)>,
//     leftover_chains_scratch: &mut Vec<u64>,
//     work_counters: &mut Vec<usize>
// ) -> Result<usize, SynthesisError> {
//     let initial_size = buckets.len();

//     // first sort the chain and split it into leftovers

//     // let bucket_index_mask = (1u64 << 32) - 1;

//     let start = std::time::Instant::now();

//     quicksort_by_index(buckets, |i, j| {
//         (decode_bucket(chains[i]).0).cmp(&decode_bucket(chains[j]).0)
//         // (chains[i] & bucket_index_mask).cmp(&(chains[j] & bucket_index_mask))
//     });

//     chains.sort_by(|&a, &b| {
//         (decode_bucket(a).0).cmp(&decode_bucket(b).0)
//         // (a & bucket_index_mask).cmp(&(b & bucket_index_mask))
//     });

//     println!("Sorting taken {:?}", start.elapsed());

//     // for c in chains.iter() {
//     //     let (bucket, pointer) = decode_bucket(*c);
//     //     println!("{}", bucket);
//     // }

//     // println!("{:?}", work_counters);

//     debug_assert_eq!(buckets.len(), chains.len());

//     // chains and buckets are now sorted by bucket number

//     let one = <G::Base as Field>::one();
//     let mut tmp = one;

//     let mut buckets_drain = buckets.drain(0..buckets.len());
//     let mut chains_drain = chains.drain(0..chains.len());

//     let b_drain_ref = &mut buckets_drain;
//     let c_drain_ref = &mut chains_drain;

//     for (bucket_index, &work_counter) in work_counters.iter().enumerate() {
//         if work_counter == 0 {
//             continue;
//         }
//         if work_counter % 2 != 0 {
//             let p = b_drain_ref.next().unwrap();
//             let c = c_drain_ref.next().unwrap();
//             leftover_buckets_scratch.push(p);
//             leftover_chains_scratch.push(c);
//         }
//         for _ in 0..(work_counter/2) {
//             // we've taken two values from chain and from bases
//             // let c0 = decode_bucket(c_drain_ref.next().unwrap());
//             // let c1 = decode_bucket(c_drain_ref.next().unwrap());
//             // assert_eq!(c0.0, bucket_index);
//             // assert_eq!(c1.0, bucket_index);
//             c_drain_ref.next();
//             c_drain_ref.next();

//             let (x0, y0) = b_drain_ref.next().unwrap();
//             let (x1, y1) = b_drain_ref.next().unwrap();

//             let mut y_diff = y1;
//             y_diff.sub_assign(&y0);

//             let mut x_diff = x1;
//             x_diff.sub_assign(&x0);

//             scratch_pad_y_diff.push(y_diff);
//             scratch_pad_x_diff.push(x_diff);
//             scratch_pad_x0_x1_y0.push((x0, x1, y0));

//             tmp.mul_assign(&x_diff);
//             prod_scratch.push(tmp);
//         }
//     }

//     drop(c_drain_ref);
//     drop(b_drain_ref);
//     drop(buckets_drain);
//     drop(chains_drain);

//     // First pass: compute [a, ab, abc, ...]

//     tmp = tmp.inverse().ok_or(SynthesisError::DivisionByZero)?;

//     // [abcd..., ..., ab, a, 1]
//     // this is just a batch inversion
//     let prod_iter = prod_scratch.drain(0..prod_scratch.len()).rev().skip(1).chain(Some(one));

//     for (x_diff_value, p) in scratch_pad_x_diff.iter_mut().rev().zip(prod_iter) {
//         let mut newtmp = tmp;
//         newtmp.mul_assign(&x_diff_value);
//         *x_diff_value = tmp;
//         x_diff_value.mul_assign(&p);

//         tmp = newtmp;
//     }

//     // let chains_len = chain.len();

//     // // so we can drain the end
//     // chains.drain((chains_len - pais_taken) .. chains_len);

//     // debug_assert!(*(chains.last().unwrap()) != invalid_bucket_index);

//     // // may be drain here is better
//     // // while let Some(idx) = chains.pop() {
//     // //     if idx != invalid_bucket_index {
//     // //         chains.push(idx);
//     // //         break;
//     // //     }
//     // // }

//     let mut scratch_pad_y_diff_drain = scratch_pad_y_diff.drain(0..scratch_pad_y_diff.len());
//     let mut scratch_pad_x_diff_drain = scratch_pad_x_diff.drain(0..scratch_pad_x_diff.len());
//     let mut scratch_pad_x0_x1_y0_drain = scratch_pad_x0_x1_y0.drain(0..scratch_pad_x0_x1_y0.len());

//     for (bucket_index, work_counter) in work_counters.iter_mut().enumerate() {
//         if *work_counter == 0 {
//             continue;
//         }
//         let new_work_size = (*work_counter / 2) + (*work_counter % 2);
//         for _ in 0..(*work_counter/2) {
//             let y_diff = scratch_pad_y_diff_drain.next().unwrap();
//             let x_diff = scratch_pad_x_diff_drain.next().unwrap();
//             let x0_x1_y0 = scratch_pad_x0_x1_y0_drain.next().unwrap();

//             let mut lambda = y_diff;
//             lambda.mul_assign(&x_diff);

//             let mut x_new = lambda;
//             x_new.square();

//             x_new.sub_assign(&x0_x1_y0.0); // - x0
//             x_new.sub_assign(&x0_x1_y0.1); // - x1

//             let mut y_new = x0_x1_y0.1; // x1
//             y_new.sub_assign(&x_new);
//             y_new.mul_assign(&lambda);
//             y_new.sub_assign(&x0_x1_y0.2);

//             let new_encoding = encode_bucket(bucket_index, buckets.len());
//             buckets.push((x_new, y_new));
//             chains.push(new_encoding);
//         }
//         *work_counter = new_work_size;
//     }

//     let mut bucket_leftover_drain = leftover_buckets_scratch.drain(0..leftover_buckets_scratch.len());
//     let bucket_leftover_drain_ref = &mut bucket_leftover_drain;

//     for l in leftover_chains_scratch.drain(0..leftover_chains_scratch.len()) {
//         let (bucket, old_index) = decode_bucket(l);
//         let new_encoding = encode_bucket(bucket, buckets.len());
//         buckets.push(bucket_leftover_drain_ref.next().unwrap());
//         chains.push(new_encoding);
//     }

//     drop(bucket_leftover_drain_ref);
//     drop(bucket_leftover_drain);

//     debug_assert!(leftover_buckets_scratch.len() == 0);
//     debug_assert!(leftover_chains_scratch.len() == 0);

//     let final_size = buckets.len();

//     assert!(initial_size >= final_size, "initial size is {}, final is {}", initial_size, final_size);

//     Ok(final_size)
// }


cfg_if! {
    if #[cfg(feature = "nightly")] {
        #[inline(always)]
        fn multiexp_inner_impl<Q, D, G, S>(
            pool: &Worker,
            bases: S,
            density_map: D,
            exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
            skip: u32,
            c: u32,
            handle_trivial: bool
        ) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
            where for<'a> &'a Q: QueryDensity,
                D: Send + Sync + 'static + Clone + AsRef<Q>,
                G: CurveAffine,
                S: SourceBuilder<G>
        {
            multiexp_inner_with_prefetch(pool, bases, density_map, exponents, skip, c, handle_trivial)
        }
    } else {
        #[inline(always)]
        fn multiexp_inner_impl<Q, D, G, S>(
            pool: &Worker,
            bases: S,
            density_map: D,
            exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
            skip: u32,
            c: u32,
            handle_trivial: bool
        ) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
            where for<'a> &'a Q: QueryDensity,
                D: Send + Sync + 'static + Clone + AsRef<Q>,
                G: CurveAffine,
                S: SourceBuilder<G>
        {
            multiexp_inner(pool, bases, density_map, exponents, skip, c, handle_trivial)
        }
    }  
}



#[cfg(feature = "nightly")]
extern crate prefetch;

#[cfg(feature = "nightly")]
fn multiexp_inner_with_prefetch<Q, D, G, S>(
    pool: &Worker,
    bases: S,
    density_map: D,
    exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>,
    skip: u32,
    c: u32,
    handle_trivial: bool
) -> WorkerFuture< <G as CurveAffine>::Projective, SynthesisError>
    where for<'a> &'a Q: QueryDensity,
          D: Send + Sync + 'static + Clone + AsRef<Q>,
          G: CurveAffine,
          S: SourceBuilder<G>
{
    use prefetch::prefetch::*;
    // Perform this region of the multiexp
    let this = {
        // This is a Pippenger’s algorithm
        pool.compute(move || {
            // Accumulate the result
            let mut acc = G::Projective::zero();

            // Build a source for the bases
            let mut bases = bases.new();

            // Create buckets to place remainders s mod 2^c,
            // it will be 2^c - 1 buckets (no bucket for zeroes)

            // Create space for the buckets
            let mut buckets = vec![<G as CurveAffine>::Projective::zero(); (1 << c) - 1];

            let zero = <G::Engine as ScalarEngine>::Fr::zero().into_repr();
            let one = <G::Engine as ScalarEngine>::Fr::one().into_repr();
            let padding = Arc::new(vec![zero]);

            let mask = 1 << c;

            // Sort the bases into buckets
            for ((&exp, &next_exp), density) in exponents.iter()
                        .zip(exponents.iter().skip(1).chain(padding.iter()))
                        .zip(density_map.as_ref().iter()) {
                // no matter what happens - prefetch next bucket
                if next_exp != zero && next_exp != one {
                    let mut next_exp = next_exp;
                    next_exp.shr(skip);
                    let next_exp = next_exp.as_ref()[0] % mask;
                    if next_exp != 0 {
                        let p: *const <G as CurveAffine>::Projective = &buckets[(next_exp - 1) as usize];
                        prefetch::<Write, High, Data, _>(p);
                    }
                    
                }
                // Go over density and exponents
                if density {
                    if exp == zero {
                        bases.skip(1)?;
                    } else if exp == one {
                        if handle_trivial {
                            bases.add_assign_mixed(&mut acc)?;
                        } else {
                            bases.skip(1)?;
                        }
                    } else {
                        // Place multiplication into the bucket: Separate s * P as 
                        // (s/2^c) * P + (s mod 2^c) P
                        // First multiplication is c bits less, so one can do it,
                        // sum results from different buckets and double it c times,
                        // then add with (s mod 2^c) P parts
                        let mut exp = exp;
                        exp.shr(skip);
                        let exp = exp.as_ref()[0] % mask;

                        if exp != 0 {
                            bases.add_assign_mixed(&mut buckets[(exp - 1) as usize])?;
                        } else {
                            bases.skip(1)?;
                        }
                    }
                }
            }

            // Summation by parts
            // e.g. 3a + 2b + 1c = a +
            //                    (a) + b +
            //                    ((a) + b) + c
            let mut running_sum = G::Projective::zero();
            for exp in buckets.into_iter().rev() {
                running_sum.add_assign(&exp);
                acc.add_assign(&running_sum);
            }

            Ok(acc)
        })
    };
    
    this
}

/// Perform multi-exponentiation. The caller is responsible for ensuring the
/// query size is the same as the number of exponents.
pub fn multiexp<Q, D, G, S>(
    pool: &Worker,
    bases: S,
    density_map: D,
    exponents: Arc<Vec<<<G::Engine as ScalarEngine>::Fr as PrimeField>::Repr>>
) -> ChunksJoiner< <G as CurveAffine>::Projective >
    where for<'a> &'a Q: QueryDensity,
          D: Send + Sync + 'static + Clone + AsRef<Q>,
          G: CurveAffine,
          S: SourceBuilder<G>
{
    let c = if exponents.len() < 32 {
        3u32
    } else {
        (f64::from(exponents.len() as u32)).ln().ceil() as u32
    };

    if let Some(query_size) = density_map.as_ref().get_query_size() {
        // If the density map has a known query size, it should not be
        // inconsistent with the number of exponents.

        assert!(query_size == exponents.len());
    }

    let mut skip = 0;
    let mut futures = Vec::with_capacity((<G::Engine as ScalarEngine>::Fr::NUM_BITS / c + 1) as usize);

    while skip < <G::Engine as ScalarEngine>::Fr::NUM_BITS {
        let chunk_future = if skip == 0 {
            multiexp_inner_impl(pool, bases.clone(), density_map.clone(), exponents.clone(), 0, c, true)
        } else {
            multiexp_inner_impl(pool, bases.clone(), density_map.clone(), exponents.clone(), skip, c, false)
        };

        futures.push(chunk_future);
        skip += c;
    }

    let join = join_all(futures);

    ChunksJoiner {
        join,
        c
    } 
}

/// Perform multi-exponentiation. The caller is responsible for ensuring the
/// query size is the same as the number of exponents.
pub fn multiexp_dense_using_futures<G>(
    pool: &Worker,
    bases: Arc<Vec<G>>,
    exponents: Arc<Vec<<<G::Engine as ScalarEngine>::Fr as PrimeField>::Repr>>
) -> ChunksJoiner< <G as CurveAffine>::Projective >
    where G: CurveAffine
{
    let c = if exponents.len() < 32 {
        3u32
    } else {
        (f64::from(exponents.len() as u32)).ln().ceil() as u32
    };

    let mut skip = 0;
    let mut futures = Vec::with_capacity((<G::Engine as ScalarEngine>::Fr::NUM_BITS / c + 1) as usize);

    while skip < <G::Engine as ScalarEngine>::Fr::NUM_BITS {
        let chunk_future = if skip == 0 {
            multiexp_dense_inner(pool, bases.clone(), exponents.clone(), 0, c, true)
        } else {
            multiexp_dense_inner(pool, bases.clone(), exponents.clone(), skip, c, false)
        };

        futures.push(chunk_future);
        skip += c;
    }

    let join = join_all(futures);

    ChunksJoiner {
        join,
        c
    } 
}

/// Perform multi-exponentiation. The caller is responsible for ensuring the
/// query size is the same as the number of exponents.
pub fn affine_multiexp<Q, D, G, S>(
    pool: &Worker,
    bases: S,
    density_map: D,
    exponents: Arc<Vec<<<G::Engine as ScalarEngine>::Fr as PrimeField>::Repr>>
) -> ChunksJoiner< <G as CurveAffine>::Projective >
    where for<'a> &'a Q: QueryDensity,
          D: Send + Sync + 'static + Clone + AsRef<Q>,
          G: CurveAffine,
          S: AccessableSourceBuilder<G>
{
    // let c = if exponents.len() < 32 {
    //     3u32
    // } else {
    //     (f64::from(exponents.len() as u32)).ln().ceil() as u32
    // };

    let c = 8u32;

    if let Some(query_size) = density_map.as_ref().get_query_size() {
        // If the density map has a known query size, it should not be
        // inconsistent with the number of exponents.

        assert!(query_size == exponents.len());
    }

    let mut skip = 0;
    let mut futures = Vec::with_capacity((<G::Engine as ScalarEngine>::Fr::NUM_BITS / c + 1) as usize);

    while skip < <G::Engine as ScalarEngine>::Fr::NUM_BITS {
        let chunk_future = if skip == 0 {
            affine_multiexp_inner(pool, bases.clone(), density_map.clone(), exponents.clone(), 0, c, true)
        } else {
            affine_multiexp_inner(pool, bases.clone(), density_map.clone(), exponents.clone(), skip, c, false)
        };

        futures.push(chunk_future);
        skip += c;
    }

    let join = join_all(futures);

    ChunksJoiner {
        join,
        c
    } 
}

/// Perform multi-exponentiation. The caller is responsible for ensuring the
/// query size is the same as the number of exponents.
pub fn dense_affine_multiexp<G>(
    pool: &Worker,
    bases: Arc<Vec<G>>,
    exponents: Arc<Vec<<<G::Engine as ScalarEngine>::Fr as PrimeField>::Repr>>
) -> ChunksJoiner< <G as CurveAffine>::Projective >
    where G: CurveAffine
{
    // let c = if exponents.len() < 32 {
    //     3u32
    // } else {
    //     (f64::from(exponents.len() as u32)).ln().ceil() as u32
    // };

    let c = 12u32;

    let mut skip = 0;
    let mut futures = Vec::with_capacity((<G::Engine as ScalarEngine>::Fr::NUM_BITS / c + 1) as usize);

    while skip < <G::Engine as ScalarEngine>::Fr::NUM_BITS {
        let chunk_future = if skip == 0 {
            dense_affine_multiexp_inner(pool, bases.clone(), exponents.clone(), 0, c, true)
        } else {
            dense_affine_multiexp_inner(pool, bases.clone(), exponents.clone(), skip, c, false)
        };

        futures.push(chunk_future);
        skip += c;
    }

    let join = join_all(futures);

    ChunksJoiner {
        join,
        c
    } 
}

/// Perform multi-exponentiation. The caller is responsible for ensuring the
/// query size is the same as the number of exponents.
pub fn dense_affine_multiexp_by_ref<G>(
    pool: &Worker,
    bases: Arc<Vec<G>>,
    exponents: Arc<Vec<<<G::Engine as ScalarEngine>::Fr as PrimeField>::Repr>>
) -> ChunksJoiner< <G as CurveAffine>::Projective >
    where G: CurveAffine
{
    let c = if exponents.len() < 32 {
        3u32
    } else {
        let log_num_exponents = (f64::from(exponents.len() as u32)).ln().ceil() as u32;

        let mut window_for_one_pass = <G::Engine as ScalarEngine>::Fr::NUM_BITS / pool.num_cpus();
        if <G::Engine as ScalarEngine>::Fr::NUM_BITS % pool.num_cpus() != 0 {
            window_for_one_pass += 1;
        }

        if window_for_one_pass > log_num_exponents {
            window_for_one_pass / 2
        } else {
            window_for_one_pass
        }
    };

    // let c = 15u32;

    let mut skip = 0;
    let mut futures = Vec::with_capacity((<G::Engine as ScalarEngine>::Fr::NUM_BITS / c + 1) as usize);

    while skip < <G::Engine as ScalarEngine>::Fr::NUM_BITS {
        let chunk_future = if skip == 0 {
            dense_affine_multiexp_inner_by_ref(pool, bases.clone(), exponents.clone(), 0, c, true)
        } else {
            dense_affine_multiexp_inner_by_ref(pool, bases.clone(), exponents.clone(), skip, c, false)
        };

        futures.push(chunk_future);
        skip += c;
    }

    let join = join_all(futures);

    ChunksJoiner {
        join,
        c
    } 
}

pub struct ChunksJoiner<G: CurveProjective> {
    join: JoinAll< WorkerFuture<G, SynthesisError> >,
    c: u32
}

impl<G: CurveProjective> Future for ChunksJoiner<G> {
    type Output = Result<G, SynthesisError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output>
    {
        let c = self.as_ref().c;
        let join = unsafe { self.map_unchecked_mut(|s| &mut s.join) };
        match join.poll(cx) {
            Poll::Ready(v) => {
                let v = join_chunks(v, c);
                return Poll::Ready(v);
            },
            Poll::Pending => {
                return Poll::Pending;
            }
        }
    }
}

impl<G: CurveProjective> ChunksJoiner<G> {
    pub fn wait(self) -> <Self as Future>::Output {
        block_on(self)
    }
}

fn join_chunks<G: CurveProjective>
    (chunks: Vec<Result<G, SynthesisError>>, c: u32) -> Result<G, SynthesisError> {
    if chunks.len() == 0 {
        return Ok(G::zero());
    }

    let mut iter = chunks.into_iter().rev();
    let higher = iter.next().expect("is some chunk result");
    let mut higher = higher?;

    for chunk in iter {
        let this = chunk?;
        for _ in 0..c {
            higher.double();
        }

        higher.add_assign(&this);
    }

    Ok(higher)
}


/// Perform multi-exponentiation. The caller is responsible for ensuring that
/// the number of bases is the same as the number of exponents.
#[allow(dead_code)]
pub fn dense_multiexp<G: CurveAffine>(
    pool: &Worker,
    bases: & [G],
    exponents: & [<<G::Engine as ScalarEngine>::Fr as PrimeField>::Repr]
) -> Result<<G as CurveAffine>::Projective, SynthesisError>
{
    if exponents.len() != bases.len() {
        return Err(SynthesisError::AssignmentMissing);
    }
    let c = if exponents.len() < 32 {
        3u32
    } else {
        (f64::from(exponents.len() as u32)).ln().ceil() as u32
    };

    dense_multiexp_inner(pool, bases, exponents, 0, c, true)
}

fn dense_multiexp_inner<G: CurveAffine>(
    pool: &Worker,
    bases: & [G],
    exponents: & [<<G::Engine as ScalarEngine>::Fr as PrimeField>::Repr],
    mut skip: u32,
    c: u32,
    handle_trivial: bool
) -> Result<<G as CurveAffine>::Projective, SynthesisError>
{   
    use std::sync::{Mutex};
    // Perform this region of the multiexp. We use a different strategy - go over region in parallel,
    // then over another region, etc. No Arc required
    let this = {
        // let mask = (1u64 << c) - 1u64;
        let this_region = Mutex::new(<G as CurveAffine>::Projective::zero());
        let arc = Arc::new(this_region);
        pool.scope(bases.len(), |scope, chunk| {
            for (base, exp) in bases.chunks(chunk).zip(exponents.chunks(chunk)) {
                let this_region_rwlock = arc.clone();
                // let handle = 
                scope.spawn(move |_| {
                    let mut buckets = vec![<G as CurveAffine>::Projective::zero(); (1 << c) - 1];
                    // Accumulate the result
                    let mut acc = G::Projective::zero();
                    let zero = <G::Engine as ScalarEngine>::Fr::zero().into_repr();
                    let one = <G::Engine as ScalarEngine>::Fr::one().into_repr();

                    for (base, &exp) in base.iter().zip(exp.iter()) {
                        // let index = (exp.as_ref()[0] & mask) as usize;

                        // if index != 0 {
                        //     buckets[index - 1].add_assign_mixed(base);
                        // }

                        // exp.shr(c as u32);

                        if exp != zero {
                            if exp == one {
                                if handle_trivial {
                                    acc.add_assign_mixed(base);
                                }
                            } else {
                                let mut exp = exp;
                                exp.shr(skip);
                                let exp = exp.as_ref()[0] % (1 << c);
                                if exp != 0 {
                                    buckets[(exp - 1) as usize].add_assign_mixed(base);
                                }
                            }
                        }
                    }

                    // buckets are filled with the corresponding accumulated value, now sum
                    let mut running_sum = G::Projective::zero();
                    for exp in buckets.into_iter().rev() {
                        running_sum.add_assign(&exp);
                        acc.add_assign(&running_sum);
                    }

                    let mut guard = match this_region_rwlock.lock() {
                        Ok(guard) => guard,
                        Err(_) => {
                            panic!("poisoned!"); 
                            // poisoned.into_inner()
                        }
                    };

                    (*guard).add_assign(&acc);
                });
        
            }
        });

        let this_region = Arc::try_unwrap(arc).unwrap();
        let this_region = this_region.into_inner().unwrap();

        this_region
    };

    skip += c;

    if skip >= <G::Engine as ScalarEngine>::Fr::NUM_BITS {
        // There isn't another region, and this will be the highest region
        return Ok(this);
    } else {
        // next region is actually higher than this one, so double it enough times
        let mut next_region = dense_multiexp_inner(
            pool, bases, exponents, skip, c, false).unwrap();
        for _ in 0..c {
            next_region.double();
        }

        next_region.add_assign(&this);

        return Ok(next_region);
    }
}

#[test]
fn test_new_multiexp_with_bls12() {
    fn naive_multiexp<G: CurveAffine>(
        bases: Arc<Vec<G>>,
        exponents: Arc<Vec<<G::Scalar as PrimeField>::Repr>>
    ) -> G::Projective
    {
        assert_eq!(bases.len(), exponents.len());

        let mut acc = G::Projective::zero();

        for (base, exp) in bases.iter().zip(exponents.iter()) {
            acc.add_assign(&base.mul(*exp));
        }

        acc
    }

    use rand::{self, Rand};
    use crate::pairing::bls12_381::Bls12;

    use self::futures::executor::block_on;

    const SAMPLES: usize = 1 << 14;

    let rng = &mut rand::thread_rng();
    let v = Arc::new((0..SAMPLES).map(|_| <Bls12 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>());
    let g = Arc::new((0..SAMPLES).map(|_| <Bls12 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>());

    let naive = naive_multiexp(g.clone(), v.clone());

    let pool = Worker::new();

    let fast = block_on(
        multiexp(
            &pool,
            (g, 0),
            FullDensity,
            v
        )
    ).unwrap();

    assert_eq!(naive, fast);
}

#[test]
#[ignore]
fn test_new_multexp_speed_with_bn256() {
    use rand::{self, Rand};
    use crate::pairing::bn256::Bn256;
    use num_cpus;

    let cpus = num_cpus::get();
    const SAMPLES: usize = 1 << 22;

    let rng = &mut rand::thread_rng();
    let v = Arc::new((0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>());
    let g = Arc::new((0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>());

    let pool = Worker::new();

    use self::futures::executor::block_on;

    let start = std::time::Instant::now();

    let _fast = block_on(
        multiexp(
            &pool,
            (g, 0),
            FullDensity,
            v
        )
    ).unwrap();


    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("Elapsed {} ns for {} samples", duration_ns, SAMPLES);
    let time_per_sample = duration_ns/(SAMPLES as f64);
    println!("Tested on {} samples on {} CPUs with {} ns per multiplication", SAMPLES, cpus, time_per_sample);
}


#[test]
fn test_dense_multiexp_vs_new_multiexp() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::bn256::Bn256;
    use num_cpus;

    // const SAMPLES: usize = 1 << 22;
    const SAMPLES: usize = 1 << 16;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new();

    let start = std::time::Instant::now();

    let dense = dense_multiexp(
        &pool, &g, &v.clone()).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for dense for {} samples", duration_ns, SAMPLES);

    use self::futures::executor::block_on;

    let start = std::time::Instant::now();

    let sparse = block_on(
        multiexp(
            &pool,
            (Arc::new(g), 0),
            FullDensity,
            Arc::new(v)
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for sparse for {} samples", duration_ns, SAMPLES);

    assert_eq!(dense, sparse);
}

#[test]
fn test_affine_multiexp() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::bn256::Bn256;
    use num_cpus;

    // const SAMPLES: usize = 1 << 20;
    const SAMPLES: usize = 1 << 10;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new_with_cpus(1);

    let bases = Arc::new(g);
    let scalars = Arc::new(v);

    use self::futures::executor::block_on;

    let _affine = block_on(
        affine_multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();
}

#[test]
fn test_multiexp_vs_affine_multiexp() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::bn256::Bn256;
    use num_cpus;

    const SAMPLES: usize = 1 << 20;
    // const SAMPLES: usize = 1 << 16;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new();

    let bases = Arc::new(g);
    let scalars = Arc::new(v);

    use self::futures::executor::block_on;

    let start = std::time::Instant::now();

    let standard = block_on(
        multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for standard multiexp for {} samples", duration_ns, SAMPLES);

    // let pool = Worker::new_with_cpus(1);

    let start = std::time::Instant::now();

    let affine = block_on(
        affine_multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for affine multiexp for {} samples", duration_ns, SAMPLES);

    // assert_eq!(standard, affine);
}

#[test]
fn test_compact_multiexp_vs_affine_multiexp() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::compact_bn256::Bn256;
    use num_cpus;

    const SAMPLES: usize = 1 << 20;
    // const SAMPLES: usize = 1 << 16;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new();

    let bases = Arc::new(g);
    let scalars = Arc::new(v);

    use self::futures::executor::block_on;

    let start = std::time::Instant::now();

    let standard = block_on(
        multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for standard multiexp for {} samples", duration_ns, SAMPLES);

    // let pool = Worker::new_with_cpus(1);

    let start = std::time::Instant::now();

    let affine = block_on(
        affine_multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for affine multiexp for {} samples", duration_ns, SAMPLES);

    // assert_eq!(standard, affine);
}

#[test]
fn test_compact_multiexp_vs_dense_affine_multiexp() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::compact_bn256::Bn256;
    use num_cpus;

    const SAMPLES: usize = 1 << 20;
    // const SAMPLES: usize = 1 << 16;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new();

    let bases = Arc::new(g);
    let scalars = Arc::new(v);

    use self::futures::executor::block_on;

    let start = std::time::Instant::now();

    let standard = block_on(
        multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for standard multiexp for {} samples", duration_ns, SAMPLES);

    // let pool = Worker::new_with_cpus(1);

    let start = std::time::Instant::now();

    let affine = block_on(
        dense_affine_multiexp(
            &pool,
            bases.clone(),
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for affine dense multiexp for {} samples", duration_ns, SAMPLES);

    // assert_eq!(standard, affine);
}

#[test]
fn test_compact_multiexp_vs_dense_affine_multiexp_by_ref() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::compact_bn256::Bn256;
    use num_cpus;

    const SAMPLES: usize = 1 << 20;
    // const SAMPLES: usize = 1 << 16;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new();

    let bases = Arc::new(g);
    let scalars = Arc::new(v);

    use self::futures::executor::block_on;

    let start = std::time::Instant::now();

    let standard = block_on(
        multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for standard multiexp for {} samples", duration_ns, SAMPLES);

    // let pool = Worker::new_with_cpus(1);

    let start = std::time::Instant::now();

    let affine = block_on(
        dense_affine_multiexp_by_ref(
            &pool,
            bases.clone(),
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for affine dense multiexp for {} samples", duration_ns, SAMPLES);

    // assert_eq!(standard, affine);
}

#[test]
fn test_dense_affine_multiexp_by_ref() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::bn256::Bn256;
    use num_cpus;

    // const SAMPLES: usize = 1 << 20;
    const SAMPLES: usize = 1 << 10;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new_with_cpus(1);

    let bases = Arc::new(g);
    let scalars = Arc::new(v);

    use self::futures::executor::block_on;

    let _affine = block_on(
        dense_affine_multiexp_by_ref(
            &pool,
            bases.clone(),
            scalars.clone()
        )
    ).unwrap();
}

#[test]
fn test_compact_multiexp_vs_dense_futures_based_multiexp() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::compact_bn256::Bn256;
    use num_cpus;

    const SAMPLES: usize = 1 << 20;
    // const SAMPLES: usize = 1 << 16;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new();

    let bases = Arc::new(g);
    let scalars = Arc::new(v);

    use self::futures::executor::block_on;

    let start = std::time::Instant::now();

    let standard = block_on(
        multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for standard multiexp for {} samples", duration_ns, SAMPLES);

    // let pool = Worker::new_with_cpus(1);

    let start = std::time::Instant::now();

    let affine = block_on(
        multiexp_dense_using_futures(
            &pool,
            bases.clone(),
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for dense compact multiexp for {} samples", duration_ns, SAMPLES);

    // assert_eq!(standard, affine);
}

#[test]
fn test_multiexp_vs_dense_futures_based_multiexp() {
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use crate::pairing::bn256::Bn256;
    use num_cpus;

    const SAMPLES: usize = 1 << 20;
    // const SAMPLES: usize = 1 << 16;
    let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

    let v = (0..SAMPLES).map(|_| <Bn256 as ScalarEngine>::Fr::rand(rng).into_repr()).collect::<Vec<_>>();
    let g = (0..SAMPLES).map(|_| <Bn256 as Engine>::G1::rand(rng).into_affine()).collect::<Vec<_>>();

    println!("Done generating test points and scalars");

    let pool = Worker::new();

    let bases = Arc::new(g);
    let scalars = Arc::new(v);

    use self::futures::executor::block_on;

    let start = std::time::Instant::now();

    let standard = block_on(
        multiexp(
            &pool,
            (bases.clone(), 0),
            FullDensity,
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for standard multiexp for {} samples", duration_ns, SAMPLES);

    // let pool = Worker::new_with_cpus(1);

    let start = std::time::Instant::now();

    let affine = block_on(
        multiexp_dense_using_futures(
            &pool,
            bases.clone(),
            scalars.clone()
        )
    ).unwrap();

    let duration_ns = start.elapsed().as_nanos() as f64;
    println!("{} ns for dense non-compact multiexp for {} samples", duration_ns, SAMPLES);

    // assert_eq!(standard, affine);
}