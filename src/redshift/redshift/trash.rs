use num::integer::*;
use num::bigint::{BigUint};
use num::{Zero, One};
use num::ToPrimitive;

use tiny_keccak::Keccak;

impl<F: PrimeField> RescueParams<F>
{
    pub fn new() -> Self {
        let S = F::S;
        let g = F::multiplicative_generator();

        let mut t = F::char();
        t.shr(S);
        let ALPHA = g.pow(t);

        //x inner is equal to p-1
        let mut x_inner = F::char();
        x_inner.div2();
        x_inner.shl(1);

        fn biguint_to_u64_array(mut v: BigUint) -> [u64; 4] {
            let m = BigUint::from(1u64) << 64;
            let mut ret = [0; 4];

            for idx in 0..4 {
                ret[idx] = (&v % &m).to_u64().expect("is guaranteed to fit");
                v >>= 64;
            }
            assert!(v.is_zero());
            ret
        }

        // fn compute_alpha_inapha(a: &BigUint) -> (u64, [u64; 4]) {
        //     let mut alpha = BigUint::from(3u64);
        //     loop {
        //         let ExtendedGcd{ gcd, x: _, y, .. } = a.extended_gcd(&alpha); 
        //         if gcd.is_one() {
        //             let alpha = alpha.to_u64().expect("Should fit in one machine word");
        //             let inalpha = biguint_to_u64_array(y);
        //             return (alpha, inalpha)
        //         }
        //         alpha += BigUint::one();
        //     }
        // }

        let mut x = BigUint::from(0u64);
        for limb in x_inner.as_ref() {
            x <<= 64;
            x += BigUint::from(*limb);
        }
        //let (RESCUE_ALPHA, RESCUE_INVALPHA) = compute_alpha_inapha(&x);
        let (RESCUE_ALPHA, RESCUE_INVALPHA) = (5, [5, 0, 0, 0]);

        let (quotient, remainder) = x.div_rem(&BigUint::from(3u64));
        assert!(remainder.is_zero());
        let BETA = g.pow(&biguint_to_u64_array(quotient));

        RescueParams{ S, ALPHA, RESCUE_ALPHA, RESCUE_INVALPHA, BETA }
    }
}

/// generation of mds_matrix is taken from https://github.com/KULeuven-COSIC/Marvellous/blob/master/instance_generator.sage
// # generate a mxm MDS matrix over F
//     @staticmethod
//     def MDS_matrix( F, m ):
//         z = F.primitive_element()
//         mat = matrix([[z^(i*j) for j in range(0, 2*m)] for i in range(0, m)])
//         return mat.echelon_form()[:, m:]
pub(crate) fn generate_mds_matrix<F: PrimeField>(_params: &RescueParams<F>) -> [[F; RESCUE_M]; RESCUE_M] {
    // TODO: Correct MDS generation; this causes horribly-biased output
    // in order to simplify output - the first index is column, the second is row
    let mut mds_matrix = [[F::zero(); RESCUE_M]; RESCUE_M * 2];
    for i in 0..RESCUE_M {
        for j in 0..(RESCUE_M * 2) {
            mds_matrix[j][i] = F::multiplicative_generator().pow([(i*j) as u64]);
        }
    }

    fn swap_rows<F: PrimeField>(matrix: &mut[[F; RESCUE_M]; RESCUE_M * 2], i: usize, j: usize ) -> () {
        if i == j {
            return;
        }

        for k in 0..(RESCUE_M * 2) {
            let temp = matrix[k][i];
            matrix[k][i] = matrix[k][j];
            matrix[k][j] = temp;
        }
    }

    //convert the resulting matrix to echelon_form
    for i in 0..RESCUE_M {
        let opt_idx = (i..RESCUE_M).find(|&k| ! mds_matrix[i][k].is_zero());
        if let Some(idx) = opt_idx {
            swap_rows(&mut mds_matrix, i, idx);
            let elem_inv = mds_matrix[i][idx].inverse().expect("should be non-zero");

            for j in (i+1)..RESCUE_M {
                let mut coef = mds_matrix[i][j];
                coef.mul_assign(&elem_inv);
                mds_matrix[i][j] = F::zero();

                for k in (i+1)..(RESCUE_M * 2) {
                    let mut temp = mds_matrix[k][idx].clone();
                    temp.mul_assign(&coef);
                    mds_matrix[k][j].sub_assign(&temp);
                }
            }
        }
    }

    //now we need to return the right half of the matrix
    let mut res = [[F::zero(); RESCUE_M]; RESCUE_M];
    res.clone_from_slice(&mds_matrix[RESCUE_M..]);
    res
}

// in https://github.com/KULeuven-COSIC/Marvellous/blob/master/instance_generator.sage there is a condition on some matrix to be invertible
// do I really need the same restriction here?

pub fn generate_constants<F: PrimeField>(iv: &str) -> [[F; RESCUE_M]; 2 * RESCUE_ROUNDS + 1] {

    let mut hasher = Keccak::new_shake256();
    hasher.update(iv.as_bytes());
    let REPR_SIZE: usize = (((F::NUM_BITS as usize)/ 64) + 1) * 8;
    let mut buf = Vec::with_capacity(REPR_SIZE);

    let mut res = [[F::zero(); RESCUE_M]; 2 * RESCUE_ROUNDS + 1];
    for i in 0..RESCUE_M {
        for j in 0..(2*RESCUE_ROUNDS +1) {

            hasher.squeeze(&mut buf[..]);
            let mut repr = F::Repr::default();
            repr.read_be(&buf[..]).expect("will read");
            res[i][j] = F::from_repr(repr).unwrap();
        }
    }
    
    res
}


/// Duplicates [`rescue_f`] in order to extract the key schedule.
fn generate_key_schedule<F: PrimeField, Params: RescueParams<F>>(
    master_key: &[F],
    params: &Params,
) -> Vec<Vec<F>> {

    let mut key_schedule = vec![];
    let state : Vec<F> = master_key.iter().cloned().collect();
    
    let constants = params.get_constants();
    let RESCUE_M = params.t();
    let RESCUE_ROUNDS = params.get_num_rescue_rounds();

    // master key should be of length RESCUE_M
    assert_eq!(master_key.len(), RESCUE_M);

    for i in 0..RESCUE_M {
        state[i].add_assign(&(constants[0][i]));
    }
    key_schedule.push(state);

    for r in 0..2 * RESCUE_ROUNDS {
        let exp = if r % 2 == 0 {
            params.rescue_invalpha()
        } else {
            &[params.rescue_alpha(), 0, 0, 0]
        };
        for entry in state.iter_mut() {
            *entry = entry.pow(exp);
        }
        for (input, output) in  mds(&state[..], params).iter().zip(state.iter()) {
            *output = *input;
        }
        for i in 0..RESCUE_M {
            state[i].add_assign(&(constants[r + 1][i]));
        }
        key_schedule.push(state);
    }

    key_schedule
}

