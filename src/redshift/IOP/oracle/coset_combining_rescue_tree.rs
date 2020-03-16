use crate::pairing::ff::{PrimeField, PrimeFieldRepr};
use crate::multicore::Worker;
use super::*;
use crate::redshift::IOP::hashes::rescue::Rescue;
use std::convert::From;

#[derive(Debug)]
pub struct FriSpecificRescueTree<F: PrimeField> {
    size: usize,
    nodes: Vec<F>,
    params: FriSpecificRescueTreeParams,
    hasher: Rescue<F>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FriSpecificRescueTreeParams {
    pub values_per_leaf: usize
}

impl From<usize> for FriSpecificRescueTreeParams {
    fn from(data: usize) -> Self {
        Self { values_per_leaf: data}
    }
}


impl<F: PrimeField> FriSpecificRescueTree<F> {
    
    fn hash_into_leaf(values: &[F], hasher: &mut Rescue<F>) -> F {
        for value in values.iter() {
            hasher.absorb(*value);
        }
        hasher.squeeze()
    }

    fn make_full_path(&self, leaf_index: usize, leaf_hash: F) -> Vec<F> {
        let mut nodes = &self.nodes[..];

        let mut path = vec![];
        path.push(leaf_hash);

        let mut idx = leaf_index;
        idx >>= 1;

        for _ in 0..log2_floor(nodes.len() / 2) {
            let half_len = nodes.len() / 2;
            let (next_level, this_level) = nodes.split_at(half_len);
            let pair_idx = idx ^ 1usize;
            let value = this_level[pair_idx];
            path.push(value);
            idx >>= 1;
            nodes = next_level;
        }

        path
    }
}

impl<F: PrimeField> Oracle<F> for FriSpecificRescueTree<F> {
    type Commitment = F;
    type Params = FriSpecificRescueTreeParams;
    type Query = CosetCombinedQuery<F>;

    fn size(&self) -> usize {
        self.size
    }

    fn create(values: &[F], params: &Self::Params) -> Self {

        assert!(params.values_per_leaf.is_power_of_two());

        let values_per_leaf = params.values_per_leaf;
        let num_leafs = values.len() / values_per_leaf;
        assert!(num_leafs.is_power_of_two());

        let num_nodes = num_leafs;

        let size = values.len();

        let mut nodes = vec![F::zero(); num_nodes];

        let hasher = Rescue::default();

        let worker = Worker::new();

        let mut leaf_hashes = vec![F::zero(); num_leafs];
        {
            worker.scope(leaf_hashes.len(), |scope, chunk| {
                for (i, lh) in leaf_hashes.chunks_mut(chunk)
                                .enumerate() {
                    scope.spawn(move |_| {
                        let base_idx = i*chunk;
                        for (j, lh) in lh.iter_mut().enumerate() {
                            let idx = base_idx + j;
                            let values_start = idx * values_per_leaf;
                            let values_end = values_start + values_per_leaf;
                            *lh = Self::hash_into_leaf(&values[values_start..values_end], &mut hasher.clone());
                        }
                    });
                }
            });
        }

        // leafs are now encoded and hashed, so let's make a tree

        let num_levels = log2_floor(num_leafs) as usize;
        let mut nodes_for_hashing = &mut nodes[..];

        // separately hash last level, which hashes leaf hashes into first nodes
        {
            let _level = num_levels-1;
            let inputs = &mut leaf_hashes[..];
            let (_, outputs) = nodes_for_hashing.split_at_mut(nodes_for_hashing.len()/2);
            assert!(outputs.len() * 2 == inputs.len());
            assert!(outputs.len().is_power_of_two());

            worker.scope(outputs.len(), |scope, chunk| {
                for (o, i) in outputs.chunks_mut(chunk)
                                .zip(inputs.chunks(chunk*2)) {
                    scope.spawn(move |_| {
                        for (o, i) in o.iter_mut().zip(i.chunks(2)) {
                            let mut hasher = hasher.clone();
                            hasher.absorb(i[0]);
                            hasher.absorb(i[1]);
                            *o = hasher.squeeze();
                        }
                    });
                }
            });
        }

        for _ in (0..(num_levels-1)).rev() {
            // do the trick - split
            let (next_levels, inputs) = nodes_for_hashing.split_at_mut(nodes_for_hashing.len()/2);
            let (_, outputs) = next_levels.split_at_mut(next_levels.len() / 2);
            assert!(outputs.len() * 2 == inputs.len());
            assert!(outputs.len().is_power_of_two());

            worker.scope(outputs.len(), |scope, chunk| {
                for (o, i) in outputs.chunks_mut(chunk)
                                .zip(inputs.chunks(chunk*2)) {
                    scope.spawn(move |_| {
                        for (o, i) in o.iter_mut().zip(i.chunks(2)) {
                            let mut hasher = hasher.clone();
                            hasher.absorb(i[0]);
                            hasher.absorb(i[1]);
                            *o = hasher.squeeze();
                        }
                    });
                }
            });

            nodes_for_hashing = next_levels;
        }

        Self {
            size: size,
            nodes: nodes,
            params: params.clone(),
            hasher: hasher,
        }
    }

    fn get_commitment(&self) -> Self::Commitment {
        self.nodes[1]
    }

    fn produce_query(&self, indexes: Range<usize>, values: &[F]) -> Self::Query {
        // we never expect that query is mis-alligned, so check it
        debug_assert!(indexes.start % self.params.values_per_leaf == 0);
        debug_assert!(indexes.len() == self.params.values_per_leaf);
        debug_assert!(indexes.end < self.size());
        debug_assert!(indexes.end < values.len());

        let query_values = Vec::from(&values[indexes.start..indexes.end]);

        let leaf_index = indexes.start / self.params.values_per_leaf;

        let pair_index = leaf_index ^ 1;

        let leaf_pair_hash = Self::hash_into_leaf(&values[(pair_index*self.params.values_per_leaf)..((pair_index+1)*self.params.values_per_leaf)], &mut self.hasher.clone());

        let path = self.make_full_path(leaf_index, leaf_pair_hash);

        CosetCombinedQuery::<F> {
            indexes: indexes,
            values: query_values,
            path: path,
        }
    }

    fn verify_query(commitment: &Self::Commitment, query: &Self::Query, params: &Self::Params) -> bool {
        if query.values().len() != params.values_per_leaf {
            return false;
        }

        let hasher = Rescue::default();

        let mut hash = Self::hash_into_leaf(query.values(), &mut hasher.clone());
        let mut idx = query.indexes().start / params.values_per_leaf;

        for el in query.path.iter() {
            let mut temp_hasher = hasher.clone();
            {
                
                if idx & 1usize == 0 {
                    temp_hasher.absorb(hash);
                    temp_hasher.absorb(*el);
                } else {
                    temp_hasher.absorb(*el);
                    temp_hasher.absorb(hash);
                }
            }
            hash = temp_hasher.squeeze();
            idx >>= 1;
        }

        &hash == commitment
    }
}

impl<F: PrimeField> PartialEq for FriSpecificRescueTree<F> {
    fn eq(&self, other: &Self) -> bool {
        self.get_commitment() == other.get_commitment()
    }
}

impl<F: PrimeField> Eq for FriSpecificRescueTree<F> {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CosetCombinedQuery<F: PrimeField> {
    indexes: Range<usize>,
    values: Vec<F>,
    path: Vec<F>,
}

impl<F: PrimeField> IopQuery<F> for CosetCombinedQuery<F> {
    fn indexes(&self) -> Range<usize> {
        self.indexes.clone()
    }

    fn values(&self) -> &[F] {
        &self.values
    }

    fn card(&self) -> usize {
        self.path.len()
    }

}

#[test]
fn make_small_iop() {
    use crate::ff::Field;
    use crate::redshift::partial_reduction_field::Fr;

    const SIZE: usize = 16;
    const VALUES_PER_LEAF: usize = 4;

    let params = FriSpecificRescueTreeParams {
        values_per_leaf: VALUES_PER_LEAF
    };

    let mut inputs = vec![];
    let mut f = Fr::one();
    for _ in 0..SIZE {
        inputs.push(f);
        f.double();
    }

    let iop = FriSpecificRescueTree::create(&inputs, &params);
    let commitment = iop.get_commitment();
    let tree_size = iop.size();
    assert!(tree_size == SIZE);
    assert!(iop.nodes.len() == (SIZE / VALUES_PER_LEAF));
    for i in 0..(SIZE / VALUES_PER_LEAF) {
        let indexes= (i*VALUES_PER_LEAF)..(VALUES_PER_LEAF + i*VALUES_PER_LEAF);
        let query = iop.produce_query(indexes, &inputs);
        let valid = FriSpecificRescueTree::verify_query(&commitment, &query, &params);
        assert!(valid, "invalid query for leaf index {}", i);
    }
}


#[test]
fn test_bench_large_fri_specific_iop() {
    use crate::ff::Field;
    use crate::redshift::partial_reduction_field::Fr;

    const SIZE: usize = 1 << (20 + 4);
    const VALUES_PER_LEAF: usize = 8;

    let params = FriSpecificRescueTreeParams {
        values_per_leaf: VALUES_PER_LEAF
    };

    let mut inputs = vec![];
    let mut f = Fr::one();
    for _ in 0..SIZE {
        inputs.push(f);
        f.double();
    }

    let iop = FriSpecificRescueTree::create(&inputs, &params);
    let commitment = iop.get_commitment();
    let tree_size = iop.size();
    assert!(tree_size == SIZE);
    assert!(iop.nodes.len() == (SIZE / VALUES_PER_LEAF));
    for i in 0..128 {
        let indexes = (i*VALUES_PER_LEAF)..(VALUES_PER_LEAF + i*VALUES_PER_LEAF); 
        let query = iop.produce_query(indexes, &inputs);
        let valid = FriSpecificRescueTree::verify_query(&commitment, &query, &params);
        assert!(valid, "invalid query for leaf index {}", i);
    }
}