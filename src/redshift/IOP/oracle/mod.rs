use crate::ff::PrimeField;
use std::convert::From;
use std::ops::Range;

pub mod coset_combining_blake2s_tree;
pub mod coset_combining_rescue_tree;

pub trait Commitment: Clone + Eq + PartialEq + std::fmt::Debug {}

pub trait Oracle<F: PrimeField>: PartialEq + Eq {
    // for now - hash of merklee tree
    type Commitment: Clone + Eq + PartialEq + std::fmt::Debug;
    // quried value alongside some proof of retrieval correctness - Merklee path for now
    type Query: IopQuery<F>;
    type Params: Clone + Eq + PartialEq + std::fmt::Debug + From<usize>;

    fn create(values: &[F], params: &Self::Params) -> Self;
    fn size(&self) -> usize;
    fn get_commitment(&self) -> Self::Commitment;
    // in the current implementation we use the "coset-combining" trick - 
    // we allow several values to be stored in the same leaf
    // that's why query may be produced to the whole range of nearby values (but they must be fit in the same leaf)
    fn verify_query(commitment: &Self::Commitment, query: &Self::Query, params: &Self::Params) -> bool;
    fn produce_query(&self, indexes: Range<usize>, values: &[F]) -> Self::Query;
}

pub trait IopQuery<F: PrimeField>: 'static + PartialEq + Eq + Clone + std::fmt::Debug {
    fn indexes(&self) -> Range<usize>;
    fn values(&self) -> &[F];
    // "card" (cardinality) is used as a countermeasure to the following kinds of attacks (however, I'm not sure how severe and critical they are)
    // assume, that we generate a query from VectorAccumulator (i.e. Merklee tree of size n), 
    // but later check the query against the oracle of smaller size (but with the same Merklee tree)
    // to protect orselves we will remember the size of oracle from which the query was borrowed 
    //and then compare it explicitely against the size of corresponding oracke during verify_query protocol
    fn card(&self) -> usize;
}

// this struct is used as an aggregator of multiple different oracles.
// as a common example comsider the following situation: we use batch version of FRI
// at a "highest" level, we have series of different oracles, and we need to open them simultaneously
// at the same set of points
#[derive(PartialEq, Eq, Clone)]
pub struct BatchedOracle<'a, F, I>
where F: PrimeField, I: Oracle<F>
{
    pub oracles: Vec<&'a I>,
    _marker_f: std::marker::PhantomData<F>,
}

impl<'a, F: PrimeField, I: Oracle<F>> BatchedOracle<'a, F, I>
{
    pub fn create(oracles: Vec<&'a I>) -> Self {
        // all of the suboracles should have the same size
        assert!(oracles.windows(2).all(|w| w[0].size() == w[1].size()));
        // and there should be at least one oracle!
        assert!(!oracles.is_empty());

        BatchedOracle{ oracles, _marker_f : std::marker::PhantomData::<F> }
    }

    pub fn size(&self) -> usize {
        self.oracles[0].size()
    }

    pub fn get_commitment(&self) -> Vec<I::Commitment> {
        self.oracles.iter().map(|x| x.get_commitment()).collect()
    }

    pub fn verify_query(commitment: &Vec<I::Commitment>, query: &Vec<I::Query>, params: &I::Params) -> bool {
        for (sub_query, sub_commitment) in query.iter().zip(commitment.iter()) {
            if !I::verify_query(sub_commitment, sub_query, params) {
                return false;
            }
        }
        true
    }

    pub fn produce_query(&self, indexes: Range<usize>, values: &Vec<&[F]>) -> Vec<I::Query> {
        self.oracles.iter().zip(values.iter()).map(|(x, val)| x.produce_query(indexes.clone(), val)).collect()
    }
}

pub fn log2_floor(num: usize) -> u32 {
    assert!(num > 0);

    let mut pow = 0;

    while (1 << (pow+1)) <= num {
        pow += 1;
    }

    pow
}
