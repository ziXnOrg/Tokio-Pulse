#![allow(unsafe_code)] // OS isolation APIs require unsafe calls

//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
//! Task isolation mechanisms for different operating systems
//!
//! This module provides OS-specific task isolation to prevent misbehaving
//! tasks from monopolizing system resources beyond their assigned tier limits.

use crate::tier_manager::TaskId;
use std::io;

#[cfg(feature = "tracing")]
use tracing::{debug, error, info, warn};

/// Result type for isolation operations
pub type IsolationResult<T> = Result<T, IsolationError>;

/// Errors that can occur during task isolation
#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    /// System call failed
    #[error("System call failed: {0}")]
    SystemCall(#[from] io::Error),

    /// Permission denied for isolation operation
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Isolation feature not available on this platform
    #[error("Isolation not supported on this platform: {0}")]
    NotSupported(String),

    /// Cgroup operation failed
    #[error("Cgroup operation failed: {0}")]
    CgroupError(String),

    /// Windows Job Object operation failed
    #[error("Job Object operation failed: {0}")]
    JobObjectError(String),

    /// macOS task policy operation failed
    #[error("Task policy operation failed: {0}")]
    TaskPolicyError(String),
}

/// Task isolation manager
pub struct TaskIsolation {
    #[cfg(target_os = "linux")]
    cgroup_manager: Option<LinuxCgroupManager>,

    #[cfg(target_os = "windows")]
    job_manager: Option<WindowsJobManager>,

    #[cfg(target_os = "macos")]
    qos_manager: Option<MacosQosManager>,
}

impl TaskIsolation {
    /// Create new task isolation manager
    pub fn new() -> IsolationResult<Self> {
        Ok(Self {
            #[cfg(target_os = "linux")]
            cgroup_manager: LinuxCgroupManager::new().ok(),

            #[cfg(target_os = "windows")]
            job_manager: WindowsJobManager::new().ok(),

            #[cfg(target_os = "macos")]
            qos_manager: MacosQosManager::new().ok(),
        })
    }

    /// Check if isolation is available on this platform
    pub fn is_available(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.cgroup_manager.is_some();

        #[cfg(target_os = "windows")]
        return self.job_manager.is_some();

        #[cfg(target_os = "macos")]
        return self.qos_manager.is_some();

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        false
    }

    /// Isolate a task to prevent resource monopolization
    pub fn isolate_task(&self, task_id: &TaskId) -> IsolationResult<()> {
        #[cfg(target_os = "linux")]
        if let Some(ref manager) = self.cgroup_manager {
            return manager.isolate_task(task_id);
        }

        #[cfg(target_os = "windows")]
        if let Some(ref manager) = self.job_manager {
            return manager.isolate_task(task_id);
        }

        #[cfg(target_os = "macos")]
        if let Some(ref manager) = self.qos_manager {
            return manager.isolate_task(task_id);
        }

        Err(IsolationError::NotSupported(
            "No isolation manager available for this platform".to_string()
        ))
    }

    /// Remove task from isolation
    pub fn release_task(&self, task_id: &TaskId) -> IsolationResult<()> {
        #[cfg(target_os = "linux")]
        if let Some(ref manager) = self.cgroup_manager {
            return manager.release_task(task_id);
        }

        #[cfg(target_os = "windows")]
        if let Some(ref manager) = self.job_manager {
            return manager.release_task(task_id);
        }

        #[cfg(target_os = "macos")]
        if let Some(ref manager) = self.qos_manager {
            return manager.release_task(task_id);
        }

        Err(IsolationError::NotSupported(
            "No isolation manager available for this platform".to_string()
        ))
    }
}

/// Linux cgroup-based isolation
#[cfg(target_os = "linux")]
pub struct LinuxCgroupManager {
    cgroup_path: std::path::PathBuf,
    cgroup_version: CgroupVersion,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
enum CgroupVersion {
    V1,
    V2,
}

#[cfg(target_os = "linux")]
impl LinuxCgroupManager {
    /// Create new cgroup manager
    pub fn new() -> IsolationResult<Self> {
        use std::fs;
        use std::path::Path;

        #[cfg(feature = "tracing")]
        info!("Initializing Linux cgroup isolation");

        // Check for cgroup v2 first (unified hierarchy)
        let cgroup_v2_path = Path::new("/sys/fs/cgroup/cgroup.controllers");
        if cgroup_v2_path.exists() {
            #[cfg(feature = "tracing")]
            debug!("Detected cgroup v2");

            let cgroup_path = Path::new("/sys/fs/cgroup/tokio-pulse").to_path_buf();

            // Create our cgroup directory if it doesn't exist
            if !cgroup_path.exists() {
                fs::create_dir_all(&cgroup_path)?;

                // Enable CPU controller
                let subtree_control = cgroup_path.join("cgroup.subtree_control");
                fs::write(&subtree_control, "+cpu").map_err(|e| {
                    IsolationError::CgroupError(format!("Failed to enable CPU controller: {}", e))
                })?;
            }

            return Ok(Self {
                cgroup_path,
                cgroup_version: CgroupVersion::V2,
            });
        }

        // Fall back to cgroup v1
        let cgroup_v1_path = Path::new("/sys/fs/cgroup/cpu");
        if cgroup_v1_path.exists() {
            #[cfg(feature = "tracing")]
            debug!("Detected cgroup v1");

            let cgroup_path = cgroup_v1_path.join("tokio-pulse");

            if !cgroup_path.exists() {
                fs::create_dir_all(&cgroup_path)?;
            }

            return Ok(Self {
                cgroup_path,
                cgroup_version: CgroupVersion::V1,
            });
        }

        Err(IsolationError::NotSupported(
            "No cgroup filesystem found".to_string()
        ))
    }

    /// Isolate task by moving it to a restricted cgroup
    pub fn isolate_task(&self, task_id: &TaskId) -> IsolationResult<()> {
        use std::fs;

        #[cfg(feature = "tracing")]
        info!(task_id = ?task_id, "Isolating task using cgroups");

        // Create isolated cgroup for this task
        let isolated_path = self.cgroup_path.join(format!("isolated-{}", task_id.0));
        fs::create_dir_all(&isolated_path)?;

        match self.cgroup_version {
            CgroupVersion::V2 => {
                // Set CPU limits (50% of one core)
                let cpu_max = isolated_path.join("cpu.max");
                fs::write(&cpu_max, "50000 100000").map_err(|e| {
                    IsolationError::CgroupError(format!("Failed to set CPU limit: {}", e))
                })?;

                // Move current thread to isolated cgroup
                let cgroup_procs = isolated_path.join("cgroup.procs");
                let tid = unsafe { libc::gettid() };
                fs::write(&cgroup_procs, tid.to_string()).map_err(|e| {
                    IsolationError::CgroupError(format!("Failed to move task to cgroup: {}", e))
                })?;
            }
            CgroupVersion::V1 => {
                // Set CPU quota (50000us per 100000us period = 50%)
                let cpu_quota = isolated_path.join("cpu.cfs_quota_us");
                fs::write(&cpu_quota, "50000").map_err(|e| {
                    IsolationError::CgroupError(format!("Failed to set CPU quota: {}", e))
                })?;

                let cpu_period = isolated_path.join("cpu.cfs_period_us");
                fs::write(&cpu_period, "100000").map_err(|e| {
                    IsolationError::CgroupError(format!("Failed to set CPU period: {}", e))
                })?;

                // Move current thread to isolated cgroup
                let tasks = isolated_path.join("tasks");
                let tid = unsafe { libc::gettid() };
                fs::write(&tasks, tid.to_string()).map_err(|e| {
                    IsolationError::CgroupError(format!("Failed to move task to cgroup: {}", e))
                })?;
            }
        }

        #[cfg(feature = "tracing")]
        debug!(task_id = ?task_id, path = ?isolated_path, "Task successfully isolated");

        Ok(())
    }

    /// Release task from isolation
    pub fn release_task(&self, task_id: &TaskId) -> IsolationResult<()> {
        use std::fs;

        #[cfg(feature = "tracing")]
        info!(task_id = ?task_id, "Releasing task from cgroup isolation");

        let isolated_path = self.cgroup_path.join(format!("isolated-{}", task_id.0));

        if !isolated_path.exists() {
            #[cfg(feature = "tracing")]
            warn!(task_id = ?task_id, "Task was not isolated");
            return Ok(());
        }

        // Move thread back to root cgroup
        let root_procs = match self.cgroup_version {
            CgroupVersion::V2 => self.cgroup_path.parent().unwrap().join("cgroup.procs"),
            CgroupVersion::V1 => self.cgroup_path.parent().unwrap().join("tasks"),
        };

        let tid = unsafe { libc::gettid() };
        fs::write(&root_procs, tid.to_string()).map_err(|e| {
            IsolationError::CgroupError(format!("Failed to move task back to root: {}", e))
        })?;

        // Remove the isolated cgroup directory
        fs::remove_dir_all(&isolated_path).map_err(|e| {
            IsolationError::CgroupError(format!("Failed to cleanup cgroup: {}", e))
        })?;

        #[cfg(feature = "tracing")]
        debug!(task_id = ?task_id, "Task successfully released from isolation");

        Ok(())
    }
}

/// Windows Job Object-based isolation
#[cfg(target_os = "windows")]
pub struct WindowsJobManager {
    job_handles: std::sync::Mutex<std::collections::HashMap<u64, windows_sys::Win32::Foundation::HANDLE>>,
}

#[cfg(target_os = "windows")]
impl WindowsJobManager {
    pub fn new() -> IsolationResult<Self> {
        #[cfg(feature = "tracing")]
        info!("Initializing Windows Job Object isolation");

        Ok(Self {
            job_handles: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub fn isolate_task(&self, task_id: &TaskId) -> IsolationResult<()> {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
            System::Threading::{
                CreateJobObjectA, SetInformationJobObject, AssignProcessToJobObject,
                GetCurrentProcess, JobObjectBasicLimitInformation,
                JOBOBJECT_BASIC_LIMIT_INFORMATION,
            },
        };

        #[cfg(feature = "tracing")]
        info!(task_id = ?task_id, "Isolating task using Windows Job Object");

        unsafe {
            // Create a job object for this task
            let job_name = format!("tokio-pulse-{}\0", task_id.0);
            let job_handle = CreateJobObjectA(
                std::ptr::null_mut(),
                job_name.as_ptr() as *const i8,
            );

            if job_handle == INVALID_HANDLE_VALUE || job_handle == 0 {
                return Err(IsolationError::JobObjectError(
                    "Failed to create job object".to_string()
                ));
            }

            // Set CPU time limit (50% of one core)
            let mut job_limits = JOBOBJECT_BASIC_LIMIT_INFORMATION {
                PerProcessUserTimeLimit: windows_sys::Win32::Foundation::LARGE_INTEGER {
                    QuadPart: 500_000_000, // 50% in 100ns units
                },
                PerJobUserTimeLimit: windows_sys::Win32::Foundation::LARGE_INTEGER {
                    QuadPart: 500_000_000,
                },
                LimitFlags: windows_sys::Win32::System::Threading::JOB_OBJECT_LIMIT_PROCESS_TIME
                    | windows_sys::Win32::System::Threading::JOB_OBJECT_LIMIT_JOB_TIME,
                MinimumWorkingSetSize: 0,
                MaximumWorkingSetSize: 0,
                ActiveProcessLimit: 1,
                Affinity: 0,
                PriorityClass: 0,
                SchedulingClass: 0,
            };

            let result = SetInformationJobObject(
                job_handle,
                JobObjectBasicLimitInformation,
                &mut job_limits as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>() as u32,
            );

            if result == 0 {
                CloseHandle(job_handle);
                return Err(IsolationError::JobObjectError(
                    "Failed to set job object limits".to_string()
                ));
            }

            // Assign current process to the job object
            let current_process = GetCurrentProcess();
            let result = AssignProcessToJobObject(job_handle, current_process);

            if result == 0 {
                CloseHandle(job_handle);
                return Err(IsolationError::JobObjectError(
                    "Failed to assign process to job object".to_string()
                ));
            }

            // Store the job handle for later cleanup
            if let Ok(mut handles) = self.job_handles.lock() {
                handles.insert(task_id.0, job_handle);
            } else {
                // If we can't store the handle, clean up immediately
                CloseHandle(job_handle);
                return Err(IsolationError::JobObjectError(
                    "Failed to store job handle".to_string()
                ));
            }

            #[cfg(feature = "tracing")]
            debug!(task_id = ?task_id, job_handle = job_handle as usize, "Task successfully isolated in job object");

            Ok(())
        }
    }

    pub fn release_task(&self, task_id: &TaskId) -> IsolationResult<()> {
        use windows_sys::Win32::Foundation::CloseHandle;

        #[cfg(feature = "tracing")]
        info!(task_id = ?task_id, "Releasing task from Job Object isolation");

        // Retrieve and close the job handle
        if let Ok(mut handles) = self.job_handles.lock() {
            if let Some(job_handle) = handles.remove(&task_id.0) {
                unsafe {
                    let result = CloseHandle(job_handle);
                    if result == 0 {
                        #[cfg(feature = "tracing")]
                        warn!(task_id = ?task_id, "Failed to close job handle, but continuing");
                    }
                }

                #[cfg(feature = "tracing")]
                debug!(task_id = ?task_id, job_handle = job_handle as usize, "Job handle successfully closed");
            } else {
                #[cfg(feature = "tracing")]
                warn!(task_id = ?task_id, "No job handle found for task");
            }
        } else {
            return Err(IsolationError::JobObjectError(
                "Failed to access job handles".to_string()
            ));
        }

        #[cfg(feature = "tracing")]
        debug!(task_id = ?task_id, "Task successfully released from job object");

        Ok(())
    }

    /// Get the number of currently tracked job handles
    pub fn active_job_count(&self) -> usize {
        self.job_handles
            .lock()
            .map(|handles| handles.len())
            .unwrap_or(0)
    }

    /// Check if a task is currently isolated
    pub fn is_task_isolated(&self, task_id: &TaskId) -> bool {
        self.job_handles
            .lock()
            .map(|handles| handles.contains_key(&task_id.0))
            .unwrap_or(false)
    }
}

/// macOS QoS-based isolation
#[cfg(target_os = "macos")]
pub struct MacosQosManager {
    isolated_threads: std::sync::Mutex<std::collections::HashSet<u64>>,
}

#[cfg(target_os = "macos")]
impl MacosQosManager {
    /// Creates a new macOS QoS manager for task isolation
    ///
    /// Initializes the QoS manager that can isolate tasks using macOS
    /// thread scheduling policies.
    ///
    /// # Errors
    ///
    /// Returns an error if QoS initialization fails
    pub fn new() -> IsolationResult<Self> {
        #[cfg(feature = "tracing")]
        info!("Initializing macOS QoS isolation");

        Ok(Self {
            isolated_threads: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }

    /// Isolates a task using macOS QoS scheduling policies
    ///
    /// Applies background QoS scheduling to reduce the task's priority
    /// and CPU scheduling weight.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The task identifier to isolate
    ///
    /// # Errors
    ///
    /// Returns an error if QoS policy application fails
    ///
    /// # Safety
    ///
    /// Uses unsafe macOS system calls that require proper thread context
    pub fn isolate_task(&self, task_id: &TaskId) -> IsolationResult<()> {
        use mach2::thread_policy::{
            thread_policy_set, THREAD_PRECEDENCE_POLICY, THREAD_THROUGHPUT_QOS_POLICY,
        };
        use libc::thread_policy_t;
        use mach2::kern_return::KERN_SUCCESS;

        #[cfg(feature = "tracing")]
        info!(task_id = ?task_id, "Isolating task using macOS QoS");

        unsafe {
            // Get current thread
            let current_thread = mach2::mach_init::mach_thread_self();

            // Set thread to background QoS (lowest priority)
            #[repr(C)]
            struct ThreadPrecedencePolicy {
                importance: i32,
            }

            let precedence_policy = ThreadPrecedencePolicy {
                importance: -15, // Lowest precedence
            };

            let result = thread_policy_set(
                current_thread,
                THREAD_PRECEDENCE_POLICY,
                &precedence_policy as *const _ as thread_policy_t,
                std::mem::size_of::<ThreadPrecedencePolicy>() as u32 / 4,
            );

            if result != KERN_SUCCESS {
                return Err(IsolationError::TaskPolicyError(format!(
                    "Failed to set thread precedence policy: {}",
                    result
                )));
            }

            // Set thread to utility QoS for background processing
            #[repr(C)]
            struct ThreadThroughputQosPolicy {
                tier: u32,
            }

            let qos_policy = ThreadThroughputQosPolicy {
                tier: 2, // Utility QoS tier
            };

            let result = thread_policy_set(
                current_thread,
                THREAD_THROUGHPUT_QOS_POLICY,
                &qos_policy as *const _ as thread_policy_t,
                std::mem::size_of::<ThreadThroughputQosPolicy>() as u32 / 4,
            );

            if result != KERN_SUCCESS {
                return Err(IsolationError::TaskPolicyError(format!(
                    "Failed to set thread QoS policy: {}",
                    result
                )));
            }

            // Track the isolated thread
            if let Ok(mut isolated_threads) = self.isolated_threads.lock() {
                isolated_threads.insert(current_thread as u64);
            }

            #[cfg(feature = "tracing")]
            debug!(task_id = ?task_id, thread = current_thread, "Task successfully isolated with background QoS");

            Ok(())
        }
    }

    /// Releases a task from QoS isolation back to normal scheduling
    ///
    /// Restores the task's thread to normal QoS class and scheduling
    /// precedence, removing isolation constraints.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The task identifier to release from isolation
    ///
    /// # Errors
    ///
    /// Returns an error if QoS policy restoration fails
    ///
    /// # Safety
    ///
    /// Uses unsafe macOS system calls that require proper thread context
    pub fn release_task(&self, task_id: &TaskId) -> IsolationResult<()> {
        use mach2::thread_policy::{
            thread_policy_set, THREAD_PRECEDENCE_POLICY, THREAD_THROUGHPUT_QOS_POLICY,
        };
        use libc::thread_policy_t;
        use mach2::kern_return::KERN_SUCCESS;

        #[cfg(feature = "tracing")]
        info!(task_id = ?task_id, "Releasing task from macOS QoS isolation");

        unsafe {
            let current_thread = mach2::mach_init::mach_thread_self();

            // Reset thread to normal precedence
            #[repr(C)]
            struct ThreadPrecedencePolicy {
                importance: i32,
            }

            let precedence_policy = ThreadPrecedencePolicy {
                importance: 0, // Normal precedence
            };

            let result = thread_policy_set(
                current_thread,
                THREAD_PRECEDENCE_POLICY,
                &precedence_policy as *const _ as thread_policy_t,
                std::mem::size_of::<ThreadPrecedencePolicy>() as u32 / 4,
            );

            if result != KERN_SUCCESS {
                #[cfg(feature = "tracing")]
                warn!(task_id = ?task_id, "Failed to reset thread precedence: {}", result);
            }

            // Reset to default QoS tier
            #[repr(C)]
            struct ThreadThroughputQosPolicy {
                tier: u32,
            }

            let qos_policy = ThreadThroughputQosPolicy {
                tier: 0, // Default tier
            };

            let result = thread_policy_set(
                current_thread,
                THREAD_THROUGHPUT_QOS_POLICY,
                &qos_policy as *const _ as thread_policy_t,
                std::mem::size_of::<ThreadThroughputQosPolicy>() as u32 / 4,
            );

            if result != KERN_SUCCESS {
                #[cfg(feature = "tracing")]
                warn!(task_id = ?task_id, "Failed to reset thread QoS: {}", result);
            } else {
                // Remove from tracking since restoration was successful
                if let Ok(mut isolated_threads) = self.isolated_threads.lock() {
                    isolated_threads.remove(&(current_thread as u64));
                }
            }

            #[cfg(feature = "tracing")]
            debug!(task_id = ?task_id, "Task successfully released from QoS isolation");

            Ok(())
        }
    }

    /// Checks if a thread is currently isolated
    ///
    /// Returns true if the specified thread ID is currently tracked
    /// as being under QoS isolation.
    ///
    /// # Arguments
    ///
    /// * `thread_id` - The thread ID to check
    pub fn is_thread_isolated(&self, thread_id: u64) -> bool {
        self.isolated_threads
            .lock()
            .map(|threads| threads.contains(&thread_id))
            .unwrap_or(false)
    }

    /// Gets the number of currently isolated threads
    ///
    /// Returns the count of threads currently under QoS isolation.
    pub fn isolated_thread_count(&self) -> usize {
        self.isolated_threads
            .lock()
            .map(|threads| threads.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_isolation_creation() {
        let isolation = TaskIsolation::new();
        assert!(isolation.is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_cgroup_detection() {
        // This test may fail in containers without cgroup access
        let result = LinuxCgroupManager::new();
        // Don't assert success since we may not have permissions
        match result {
            Ok(_) => println!("Cgroup isolation available"),
            Err(e) => println!("Cgroup isolation unavailable: {}", e),
        }
    }

    #[test]
    fn test_task_isolation_interface() {
        let isolation = TaskIsolation::new().unwrap();
        let task_id = TaskId(12345);

        // Test isolation interface (may fail without permissions)
        let _ = isolation.isolate_task(&task_id);
        let _ = isolation.release_task(&task_id);
    }
}