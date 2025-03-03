use std::{borrow::Borrow, cell::LazyCell, marker::PhantomData, sync::Arc};
use typenum::{Prod, Sum, Unsigned, U1, U8};

use crate::node::{Branch, EmptyLeaf, Hasher, Leaf, Node};

/// Define the empty tree array size as (HASH_SIZE * 8) + 1
type TreeSize = Sum<Prod<U8, typenum::U32>, U1>;

/// Merkle sum sparse merkle tree.
/// * `KVStore` - Key value store for nodes.
/// * `HASH_SIZE` - size of the hash digest in bytes.
/// * `H` - Hasher that will be used to hash nodes.
pub struct MSSMT<KVStore: Db<HASH_SIZE, H>, const HASH_SIZE: usize, H: Hasher<HASH_SIZE> + Clone> {
    db: KVStore,
    pub empty_tree_root_hash: [u8; HASH_SIZE],
    empty_tree: Arc<[Node<HASH_SIZE, H>; TreeSize::USIZE]>,
    _phantom: PhantomData<H>,
}

/// Helper struct to create an empty mssmt.
pub struct TreeBuilder<const HASH_SIZE: usize, H: Hasher<HASH_SIZE> + Clone>(PhantomData<H>);

impl<const HASH_SIZE: usize, H: Hasher<HASH_SIZE> + Clone> TreeBuilder<HASH_SIZE, H> {
    #[allow(clippy::declare_interior_mutable_const)]
    const EMPTY_TREE: LazyCell<Arc<[Node<HASH_SIZE, H>; TreeSize::USIZE]>> =
        LazyCell::new(|| Arc::new(Self::build_tree()));

    /// Gets an empty mssmt.
    pub fn empty_tree() -> Arc<[Node<HASH_SIZE, H>; TreeSize::USIZE]> {
        #[allow(clippy::borrow_interior_mutable_const)]
        Self::EMPTY_TREE.clone()
    }

    /// builds the empty tree
    fn build_tree() -> [Node<HASH_SIZE, H>; TreeSize::USIZE] {
        let max_height = HASH_SIZE * 8;
        let mut empty_tree = Vec::with_capacity(max_height + 1);
        let empty_leaf = Node::<HASH_SIZE, H>::Empty(EmptyLeaf::new());
        empty_tree.push(empty_leaf);

        for i in 1..=max_height {
            empty_tree.push(Node::new_branch(
                empty_tree[i - 1].clone(),
                empty_tree[i - 1].clone(),
            ));
        }
        empty_tree.reverse();

        let Node::Branch(_branch) = &empty_tree[0] else {
            panic!("Root should be a branch")
        };

        empty_tree
            .try_into()
            .unwrap_or_else(|_| panic!("Incorrect array size"))
    }

    /// Builds a new MSSMT object from the empty tree.
    pub fn build<KVStore: Db<HASH_SIZE, H>>(db: KVStore) -> MSSMT<KVStore, HASH_SIZE, H> {
        MSSMT::new_with_tree(db, Self::build_tree())
    }
}

/// Store for the tree nodes
/// 
/// This trait must be implemented by any storage backend used with the tree.
/// It provides the basic operations needed to store and retrieve nodes.
pub trait Db<const HASH_SIZE: usize, H: Hasher<HASH_SIZE> + Clone> {
    fn get_root_node(&self) -> Branch<HASH_SIZE, H>;
    fn get_branch(&self, key: &[u8; HASH_SIZE]) -> Option<Branch<HASH_SIZE, H>>;
    fn get_leaf(&self, key: &[u8; HASH_SIZE]) -> Option<Leaf<HASH_SIZE, H>>;
    fn insert_leaf(&mut self, leaf: Leaf<HASH_SIZE, H>);
    fn insert_branch(&mut self, branch: Branch<HASH_SIZE, H>);
    fn update_root(&mut self, root: Branch<HASH_SIZE, H>);
    fn delete_branch(&mut self, key: &[u8; HASH_SIZE]);
    fn delete_leaf(&mut self, key: &[u8; HASH_SIZE]);
}

fn bit_index(index: usize, key: &[u8]) -> u8 {
    // `index as usize / 8` to get the index of the interesting byte
    // `index % 8` to get the interesting bit index in the previously selected byte
    // right shift it and keep only this interesting bit with & 1.
    (key[index / 8] >> (index % 8)) & 1
}

impl<KVStore: Db<HASH_SIZE, H>, const HASH_SIZE: usize, H: Hasher<HASH_SIZE> + Clone>
    MSSMT<KVStore, HASH_SIZE, H>
{
    /// Creates a new mssmt. This will build an empty tree which will involve a lot of hashing.
    pub fn new(mut db: KVStore) -> Self {
        let empty_tree = TreeBuilder::empty_tree();
        let Node::Branch(branch) = empty_tree.as_ref()[0].clone() else {
            panic!("Root should be a branch")
        };
        let empty_tree_root_hash = branch.hash();
        db.update_root(branch);
        Self {
            db,
            empty_tree_root_hash,
            empty_tree,
            _phantom: PhantomData,
        }
    }

    /// Creates a new mssmt from an already built empty tree. No hashing involved.
    pub fn new_with_tree(
        mut db: KVStore,
        empty_tree: [Node<HASH_SIZE, H>; TreeSize::USIZE],
    ) -> Self {
        let Node::Branch(branch) = empty_tree[0].clone() else {
            panic!("Root should be a branch")
        };
        let empty_tree_root_hash = branch.hash();
        db.update_root(branch);
        Self {
            db,
            empty_tree_root_hash,
            empty_tree: Arc::new(empty_tree),
            _phantom: PhantomData,
        }
    }

    /// Max height of the tree
    pub const fn max_height() -> usize {
        HASH_SIZE * 8
    }

    /// Root node of the tree.
    pub fn root(&self) -> Branch<HASH_SIZE, H> {
        self.db.get_root_node()
    }

    pub fn get_leaf_from_top(&self, key: [u8; HASH_SIZE]) -> Leaf<HASH_SIZE, H> {
        let mut current_branch = Node::Branch(self.db.get_root_node());
        for i in 0..Self::max_height() {
            if bit_index(i, &key) == 0 {
                let (left, _) = self.get_children(i, current_branch.hash());
                current_branch = left;
            } else {
                let (_, right) = self.get_children(i, current_branch.hash());
                current_branch = right;
            }
        }
        match current_branch {
            Node::Leaf(leaf) => leaf,
            Node::Branch(_) => panic!("expected leaf found branch"),
            Node::Empty(_) => panic!("Empty node"),
        }
    }

    /// Get the children of a node from the key.
    pub fn get_children(
        &self,
        height: usize,
        key: [u8; HASH_SIZE],
    ) -> (Node<HASH_SIZE, H>, Node<HASH_SIZE, H>) {
        let get_node = |height: usize, key: [u8; HASH_SIZE]| {
            if key == self.empty_tree[height].hash() {
                self.empty_tree[height].clone()
            } else if let Some(node) = self.db.get_branch(&key) {
                Node::Branch(node)
            } else if let Some(leaf) = self.db.get_leaf(&key) {
                Node::Leaf(leaf)
            } else {
                self.empty_tree[height].clone()
            }
        };
        let node = get_node(height, key);
        if key != self.empty_tree[height].hash() && node.hash() == self.empty_tree[height].hash() {
            panic!("node not found")
        }
        if let Node::Branch(branch) = node {
            (
                get_node(height + 1, branch.left().hash()),
                get_node(height + 1, branch.right().hash()),
            )
        } else {
            panic!("Should be a branch node")
        }
    }

    /// Walk down the tree from the root node to the node.
    /// * `for_each` - Closure that is executed at each step of the traversal of the tree.
    pub fn walk_down(
        &self,
        key: [u8; HASH_SIZE],
        mut for_each: impl FnMut(usize, &Node<HASH_SIZE, H>, Node<HASH_SIZE, H>, Node<HASH_SIZE, H>),
    ) -> Node<HASH_SIZE, H> {
        let mut current = Node::Branch(self.db.get_root_node());
        for i in 0..Self::max_height() {
            let (left, right) = self.get_children(i, current.hash());
            let (next, sibling) = if bit_index(i, &key) == 0 {
                (left, right)
            } else {
                (right, left)
            };
            for_each(i, &next, sibling, current);
            current = next;
        }
        match current {
            Node::Leaf(leaf) => Node::Leaf(leaf),
            Node::Branch(_) => panic!("expected leaf found branch"),
            Node::Empty(empty) => Node::Empty(empty),
        }
    }

    /// Walk up the tree from the node to the root node.
    /// * `key` - key of the node we want to reach.
    /// * `start` - starting leaf.
    /// * `siblings` - All the sibling nodes on the path (from the leaf to the target node).
    /// * `for_each` - Closure that is executed at each step of the traversal of the tree.
    ///     * `height: usize` - current height in the tree
    ///     * `current: &Node<HASH_SIZE, H>` - current node on the way to the asked node
    ///     * `sibling: &Node<HASH_SIZE, H>` - sibling node of the current node on the way to the asked node
    ///     * `parent: &Node<HASH_SIZE, H>` - parent node of the current node on the way to the asked node
    pub fn walk_up(
        &self,
        key: [u8; HASH_SIZE],
        start: Leaf<HASH_SIZE, H>,
        siblings: Vec<Arc<Node<HASH_SIZE, H>>>,
        mut for_each: impl FnMut(usize, &Node<HASH_SIZE, H>, &Node<HASH_SIZE, H>, &Node<HASH_SIZE, H>),
    ) -> Branch<HASH_SIZE, H> {
        let mut current = Arc::new(Node::Leaf(start));
        for i in (0..Self::max_height()).rev() {
            let sibling = siblings[Self::max_height() - 1 - i].clone();
            let parent = if bit_index(i, &key) == 0 {
                Node::from((current.clone(), sibling.clone()))
            } else {
                Node::from((sibling.clone(), current.clone()))
            };
            for_each(i, &current, &sibling, &parent);
            current = Arc::new(parent);
        }
        if let Node::Branch(current) = current.borrow() {
            current.clone()
        } else {
            panic!("Shouldn't end on a leaf");
        }
    }

    /// Insert a leaf in the tree.
    pub fn insert(&mut self, key: [u8; HASH_SIZE], leaf: Leaf<HASH_SIZE, H>) {
        let mut prev_parents = Vec::with_capacity(Self::max_height());
        let mut siblings = Vec::with_capacity(Self::max_height());

        self.walk_down(key, |_, _next, sibling, parent| {
            prev_parents.push(parent.hash());
            siblings.push(Arc::new(sibling));
        });
        prev_parents.reverse();
        siblings.reverse();

        // Create a vector to store operations we'll perform after walk_up
        let mut branches_delete = Vec::new();
        let mut branches_insertion = Vec::new();
        let root = self.walk_up(
            key,
            leaf.clone(),
            siblings,
            |height, _current, _sibling, parent| {
                let prev_parent = prev_parents[Self::max_height() - height - 1];
                if prev_parent != self.empty_tree[height].hash() {
                    branches_delete.push(prev_parent);
                }
                if parent.hash() != self.empty_tree[height].hash() {
                    if let Node::Branch(parent) = parent {
                        branches_insertion.push(parent.clone());
                    }
                }
            },
        );

        for branch in branches_insertion {
            self.db.insert_branch(branch);
        }
        // Perform the database operations after walk_up
        for key in branches_delete {
            self.db.delete_branch(&key);
        }

        self.db.insert_leaf(leaf);
        self.db.update_root(root);
    }
}
