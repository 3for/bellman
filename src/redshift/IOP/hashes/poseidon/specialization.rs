use crate::ff::{Field, PrimeField};
use super::{PoseidonHashParams, scalar_product};


#[macro_export]
macro_rules! construct_sponge {
	( $(#[$attr:meta])* $visibility:vis struct $name:ident ( $n_rate:tt, $n_capacity: tt, $another_name: ident ); ) => {
		/// Little-endian large integer type
		$(#[$attr])*
        $visibility struct $name<'a, Params: PoseidonHashParams>
        {
            params: &'a Params,
            internal_state: [Params::Fr; $n_rate + $n_capacity],
            mode: $another_name<Params::Fr>
        }

        #[derive(Clone)]
        enum $another_name<Fr: PrimeField> {
            AccumulatingToAbsorb(usize, [Fr; $n_rate]),
            SqueezedInto(usize, [Fr; $n_rate])
        }

        impl<Fr: PrimeField> Copy for $another_name<Fr> {}

        impl<'a, Params: PoseidonHashParams> Clone for $name<'a, Params> {
            fn clone(&self) -> Self {
                Self {
                    params: self.params,
                    internal_state: self.internal_state,
                    mode: self.mode
                }
            }
        }

        impl<'a, Params: PoseidonHashParams> $name<'a, Params> {
            pub fn new(
                params: &'a Params
            ) -> Self 
            {
                assert!(params.rate() == $n_rate, "rate is invalid for specialization");
                assert!(params.capacity() == $n_capacity, "capacity is invalid for specialization");
                
                let op = $another_name::AccumulatingToAbsorb(0, [Params::Fr::zero(); $n_rate]);

                Self {
                    params,
                    internal_state: [Params::Fr::zero(); $n_rate + $n_capacity],
                    mode: op
                }
            }

            fn poseidon_duplex(
                params: &Params,
                internal_state: [Params::Fr; $n_rate + $n_capacity],
            ) -> [Params::Fr; $n_rate + $n_capacity] 
            {
                let mut state = internal_state;
                debug_assert!(params.num_full_rounds() % 2 == 0);
                let half_of_full_rounds = params.num_full_rounds() / 2;
                let mut mds_application_scratch = [Params::Fr::zero(); $n_rate + $n_capacity];
                debug_assert_eq!(state.len(), params.state_width() as usize);

                const LAST_ELEM_IDX: usize = $n_rate + $n_capacity - 1;

                // full rounds
                for round in 0..half_of_full_rounds {
                    let round_constants = params.round_constants(round);
                
                    // add round constatnts
                    for (s, c)  in state.iter_mut()
                                .zip(round_constants.iter()) {
                        s.add_assign(c);
                    }

                    apply(&mut state[..], params.sbox_type());

                    // mul state by MDS
                    for (row, place_into) in mds_application_scratch.iter_mut()
                                                    .enumerate() {
                        let tmp = scalar_product::<Params::Fr>(& state[..], params.mds_matrix_row(row as u32));                           
                        *place_into = tmp;
                    }

                    state = mds_application_scratch;
                }

                // partial rounds

                for round in half_of_full_rounds..(params.num_partial_rounds() + half_of_full_rounds){
                    let round_constants = params.round_constants(round);
                
                    // add round constatnts
                    for (s, c)  in state.iter_mut()
                                .zip(round_constants.iter()) {
                        s.add_assign(c);
                    }

                    apply(&mut state[..], params.sbox_type());

                    // mul state by MDS
                    for (row, place_into) in mds_application_scratch.iter_mut()
                                                    .enumerate() {
                        let tmp = scalar_product::<Params::Fr>(& state[..], params.mds_matrix_row(row as u32));
                        *place_into = tmp;                               
                    }

                    state = mds_application_scratch;
                }

                // full rounds
                for round in (params.num_partial_rounds() + half_of_full_rounds)..(params.num_partial_rounds() + params.num_full_rounds()) {
                    let round_constants = params.round_constants(round);
                
                    // add round constatnts
                    for (s, c)  in state.iter_mut()
                                .zip(round_constants.iter()) {
                        s.add_assign(c);
                    }

                    apply(&mut state[..], params.sbox_type());

                    // mul state by MDS
                    for (row, place_into) in mds_application_scratch.iter_mut()
                                                    .enumerate() {
                        let tmp = scalar_product::<Params::Fr>(& state[..], params.mds_matrix_row(row as u32));                           
                        *place_into = tmp;
                    }

                    state = mds_application_scratch;
                }

                state
            }

            pub fn absorb(
                &mut self,
                value: Params::Fr
            ) {
                match self.mode {
                    $another_name::AccumulatingToAbsorb(ref mut len, ref mut into) => {
                        // two cases
                        // either we have accumulated enough already and should to 
                        // a mimc round before accumulating more, or just accumulate more
                        if *len < $n_rate {
                            into[*len] = value;
                            *len += 1;
                        } else {
                            for i in 0..$n_rate {
                                self.internal_state[i].add_assign(&into[i]);
                            }

                            *len = 0;

                            self.internal_state = Self::poseidon_duplex(&*self.params, self.internal_state);
                        }
                    },
                    $another_name::SqueezedInto(_, _) => {
                        // we don't need anything from the output, so it's dropped

                        let mut s = [Params::Fr::zero(); $n_rate];
                        s[0] = value;

                        let op = $another_name::AccumulatingToAbsorb(1, s);
                        self.mode = op;
                    }
                }
            }

            pub fn squeeze(
                &mut self,
            ) -> Params::Fr {
                match self.mode {
                    $another_name::AccumulatingToAbsorb(len, ref mut into) => {
                        if len < $n_rate {
                            for i in len..$n_rate {
                                debug_assert!(into[i].is_zero());
                            }
                        }

                        // two cases
                        // either we have accumulated enough already and should to 
                        // a mimc round before accumulating more, or just accumulate more
                        for i in 0..len {
                            self.internal_state[i].add_assign(&into[i]);
                        }

                        self.internal_state = Self::poseidon_duplex(&*self.params, self.internal_state);

                        // we don't take full internal state, but only the rate
                        let mut sponge_output = [Params::Fr::zero(); $n_rate];
                        sponge_output.copy_from_slice(&self.internal_state[0..$n_rate]);

                        let output = sponge_output[0];

                        let op = $another_name::SqueezedInto(1, sponge_output);
                        self.mode = op;

                        return output;
                    },

                    $another_name::SqueezedInto(ref mut len, ref mut into) => {
                        if *len == $n_rate {
                            self.internal_state = Self::poseidon_duplex(&*self.params, self.internal_state);

                            let mut sponge_output = [Params::Fr::zero(); $n_rate];
                            sponge_output.copy_from_slice(&self.internal_state[0..$n_rate]);

                            let output = sponge_output[0];

                            let op = $another_name::SqueezedInto(1, sponge_output);
                            self.mode = op;

                            return output;
                        }

                        let output = into[*len];
                        *len += 1;

                        return output;
                    }
                }
            }
        }
    }
}