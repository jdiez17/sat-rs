//! # Module providing addressable object support and a manager for them
//!
//! Each addressable object can be identified using an [object ID][ObjectId].
//! The [system object][ManagedSystemObject] trait also allows storing these objects into the
//! [object manager][ObjectManager]. They can then be retrieved and casted back to a known type
//! using the object ID.
//!
//! # Examples
//!
//! ```
//! use std::any::Any;
//! use std::error::Error;
//! use launchpad::core::objects::{ManagedSystemObject, ObjectId, ObjectManager, SystemObject};
//!
//! struct ExampleSysObj {
//!     id: ObjectId,
//!     dummy: u32,
//!     was_initialized: bool,
//! }
//!
//! impl ExampleSysObj {
//!     fn new(id: ObjectId, dummy: u32) -> ExampleSysObj {
//!         ExampleSysObj {
//!             id,
//!             dummy,
//!             was_initialized: false,
//!         }
//!     }
//! }
//!
//! impl SystemObject for ExampleSysObj {
//!     fn as_any(&self) -> &dyn Any {
//!         self
//!     }
//!
//!     fn get_object_id(&self) -> &ObjectId {
//!         &self.id
//!     }
//!
//!     fn initialize(&mut self) -> Result<(), Box<dyn Error>> {
//!         self.was_initialized = true;
//!             Ok(())
//!         }
//!     }
//!
//! impl ManagedSystemObject for ExampleSysObj {}
//!
//!
//!  let mut obj_manager = ObjectManager::default();
//!  let obj_id = ObjectId { id: 0, name: "Example 0"};
//!  let example_obj = ExampleSysObj::new(obj_id, 42);
//!  obj_manager.insert(Box::new(example_obj));
//!  let obj_back_casted: Option<&ExampleSysObj> = obj_manager.get(&obj_id);
//!  let example_obj = obj_back_casted.unwrap();
//!  assert_eq!(example_obj.id, obj_id);
//!  assert_eq!(example_obj.dummy, 42);
//! ```

use std::any::Any;
use std::collections::HashMap;
use std::error::Error;

#[derive(PartialEq, Eq, Hash, Copy, Clone, Debug)]
pub struct ObjectId {
    pub id: u32,
    pub name: &'static str,
}

/// Each object which is stored inside the [object manager][ObjectManager] needs to implemented
/// this trait
pub trait SystemObject {
    fn as_any(&self) -> &dyn Any;
    fn get_object_id(&self) -> &ObjectId;
    fn initialize(&mut self) -> Result<(), Box<dyn Error>>;
}

pub trait ManagedSystemObject: SystemObject + Any + Send {}

/// Helper module to manage multiple [ManagedSystemObjects][ManagedSystemObject] by mapping them
/// using an [object ID][ObjectId]
pub struct ObjectManager {
    obj_map: HashMap<ObjectId, Box<dyn ManagedSystemObject>>,
}

impl Default for ObjectManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectManager {
    pub fn new() -> ObjectManager {
        ObjectManager {
            obj_map: HashMap::new(),
        }
    }
    pub fn insert(&mut self, sys_obj: Box<dyn ManagedSystemObject>) -> bool {
        let obj_id = sys_obj.get_object_id();
        if self.obj_map.contains_key(obj_id) {
            return false;
        }
        self.obj_map.insert(*obj_id, sys_obj).is_none()
    }

    /// Initializes all System Objects in the hash map and returns the number of successful
    /// initializations
    pub fn initialize(&mut self) -> Result<u32, Box<dyn Error>> {
        let mut init_success = 0;
        for val in self.obj_map.values_mut() {
            if val.initialize().is_ok() {
                init_success += 1
            }
        }
        Ok(init_success)
    }

    /// Retrieve an object stored inside the manager. The type to retrieve needs to be explicitly
    /// passed as a generic parameter
    pub fn get<T: Any>(&self, key: &ObjectId) -> Option<&T> {
        self.obj_map
            .get(key)
            .and_then(|o| o.as_ref().as_any().downcast_ref::<T>())
    }
}

#[cfg(test)]
mod tests {
    use crate::core::objects::{ManagedSystemObject, ObjectId, ObjectManager, SystemObject};
    use std::any::Any;
    use std::error::Error;
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct ExampleSysObj {
        id: ObjectId,
        dummy: u32,
        was_initialized: bool,
    }

    impl ExampleSysObj {
        fn new(id: ObjectId, dummy: u32) -> ExampleSysObj {
            ExampleSysObj {
                id,
                dummy,
                was_initialized: false,
            }
        }
    }

    impl SystemObject for ExampleSysObj {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn get_object_id(&self) -> &ObjectId {
            &self.id
        }

        fn initialize(&mut self) -> Result<(), Box<dyn Error>> {
            self.was_initialized = true;
            Ok(())
        }
    }

    impl ManagedSystemObject for ExampleSysObj {}

    struct OtherExampleObject {
        id: ObjectId,
        string: String,
        was_initialized: bool,
    }

    impl SystemObject for OtherExampleObject {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn get_object_id(&self) -> &ObjectId {
            &self.id
        }

        fn initialize(&mut self) -> Result<(), Box<dyn Error>> {
            self.was_initialized = true;
            Ok(())
        }
    }

    impl ManagedSystemObject for OtherExampleObject {}

    #[test]
    fn test_obj_manager_simple() {
        let mut obj_manager = ObjectManager::default();
        let expl_obj_id = ObjectId {
            id: 0,
            name: "Example 0",
        };
        let example_obj = ExampleSysObj::new(expl_obj_id, 42);
        assert!(obj_manager.insert(Box::new(example_obj)));
        let res = obj_manager.initialize();
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), 1);
        let obj_back_casted: Option<&ExampleSysObj> = obj_manager.get(&expl_obj_id);
        assert!(obj_back_casted.is_some());
        let expl_obj_back_casted = obj_back_casted.unwrap();
        assert_eq!(expl_obj_back_casted.dummy, 42);
        assert!(expl_obj_back_casted.was_initialized);

        let second_obj_id = ObjectId {
            id: 12,
            name: "Example 1",
        };
        let second_example_obj = OtherExampleObject {
            id: second_obj_id,
            string: String::from("Hello Test"),
            was_initialized: false,
        };

        assert!(obj_manager.insert(Box::new(second_example_obj)));
        let res = obj_manager.initialize();
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), 2);
        let obj_back_casted: Option<&OtherExampleObject> = obj_manager.get(&second_obj_id);
        assert!(obj_back_casted.is_some());
        let expl_obj_back_casted = obj_back_casted.unwrap();
        assert_eq!(expl_obj_back_casted.string, String::from("Hello Test"));
        assert!(expl_obj_back_casted.was_initialized);

        let existing_obj_id = ObjectId {
            id: 12,
            name: "Example 1",
        };
        let invalid_obj = OtherExampleObject {
            id: existing_obj_id,
            string: String::from("Hello Test"),
            was_initialized: false,
        };

        assert!(!obj_manager.insert(Box::new(invalid_obj)));
    }

    #[test]
    fn object_man_threaded() {
        let obj_manager = Arc::new(Mutex::new(ObjectManager::new()));
        let expl_obj_id = ObjectId {
            id: 0,
            name: "Example 0",
        };
        let example_obj = ExampleSysObj::new(expl_obj_id, 42);
        let second_obj_id = ObjectId {
            id: 12,
            name: "Example 1",
        };
        let second_example_obj = OtherExampleObject {
            id: second_obj_id,
            string: String::from("Hello Test"),
            was_initialized: false,
        };

        let mut obj_man_handle = obj_manager.lock().expect("Mutex lock failed");
        assert!(obj_man_handle.insert(Box::new(example_obj)));
        assert!(obj_man_handle.insert(Box::new(second_example_obj)));
        let res = obj_man_handle.initialize();
        std::mem::drop(obj_man_handle);
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), 2);
        let obj_man_0 = obj_manager.clone();
        let jh0 = thread::spawn(move || {
            let locked_man = obj_man_0.lock().expect("Mutex lock failed");
            let obj_back_casted: Option<&ExampleSysObj> = locked_man.get(&expl_obj_id);
            assert!(obj_back_casted.is_some());
            let expl_obj_back_casted = obj_back_casted.unwrap();
            assert_eq!(expl_obj_back_casted.dummy, 42);
            assert!(expl_obj_back_casted.was_initialized);
            std::mem::drop(locked_man)
        });

        let jh1 = thread::spawn(move || {
            let locked_man = obj_manager.lock().expect("Mutex lock failed");
            let obj_back_casted: Option<&OtherExampleObject> = locked_man.get(&second_obj_id);
            assert!(obj_back_casted.is_some());
            let expl_obj_back_casted = obj_back_casted.unwrap();
            assert_eq!(expl_obj_back_casted.string, String::from("Hello Test"));
            assert!(expl_obj_back_casted.was_initialized);
            std::mem::drop(locked_man)
        });
        jh0.join().expect("Joining thread 0 failed");
        jh1.join().expect("Joining thread 1 failed");
    }
}
